use crate::display::auxiliary::{
    feature_presentation, feature_record_index_at_display_point, genomic_distance_to_interval,
    gwas_record_index_at_point, interaction_record_index_at_position,
    measured_feature_label_widths, mutation_record_index_at_position,
    pinned_gene_layout_maximum_rows, quantitative_record_index_at_position,
    quantitative_track_y_at_position,
};
use crate::display::hit_test::{
    JUNCTION_ARC_MINIMUM_ACQUISITION_RADIUS, JunctionArcHitTestResult,
    alignment_lane_in_pane_layout,
};
use crate::display::primary_pinned_gene_id;
use crate::inspection::gene_transcripts::gene_transcript_identifiers;
use crate::inspection::model::{
    CopyNumberInspectorSnapshot, CopyNumberSelection, EvidenceSelection, FeatureInspectorSnapshot,
    FeatureSelection, GeneTranscriptsSummary, GwasInspectorSnapshot, GwasSelection,
    InteractionInspectorSnapshot, InteractionSelection, MutationInspectorSnapshot,
    MutationSelection, QuantitativeInspectorSnapshot, QuantitativeOperandSnapshot,
    QuantitativeSelection, VariantAlignmentEvidence, VariantInspectorSnapshot, VariantSelection,
    next_evidence_index,
};
use crate::inspection::rna::selected_supplementary_chain;
use crate::inspection::variant::variant_allele_evidence_summary;
use crate::ui::{TrackFrame, TrackFrameKey};
use crate::workspace::tracks::replace_panel_track_selection;
use crate::{
    JunctionArcHover, JunctionArcKey, SashimiHitTestGate, Viewer, alignment_gpu_bundle_is_current,
    layout_junction_component, native_junction_interaction_token, ui,
};
use rgv_alignment_scene::{
    AlignmentComponent, AlignmentHit, AlignmentTraversalDirection, CpuAlignmentProjector,
    SummaryPageFamily, SummarySemanticComponent,
};
use rgv_automation::AutomationSelectionGranularity;
use rgv_core::{
    CopyNumberTrackSettings, GenomicInterval, GwasTrackSettings, InteractionTrackSettings, PanelId,
    QuantitativeTrackSettings, TrackId, TrackKind, VariantTrackSettings,
};
use rgv_engine::{AlignmentIndividualReadCapability, TrackSnapshotData};
use rgv_layout::{
    COPY_NUMBER_HEADER_HEIGHT, JunctionArcComponent, MUTATION_HEADER_HEIGHT, ObjectIdAllocator,
    TrackLayoutOptions, alignment_read_name_hash, copy_number_sample_order,
    layout_presented_features,
};
use std::sync::Arc;

impl Viewer {
    pub(crate) fn visible_track_frame_at_screen_y(
        &self,
        screen_y: f32,
        predicate: impl Fn(&TrackFrame) -> bool,
    ) -> Option<(&TrackFrame, f32)> {
        let panel = self.session.panels.active();
        panel
            .pane_layout
            .annotation
            .items
            .iter()
            .chain(panel.pane_layout.reads.items.iter())
            .find_map(|item| {
                let frame = &item.frame;
                let read_panel = frame.channel == ui::TrackPane::Reads;
                (item.effective_content_visible && predicate(frame))
                    .then(|| {
                        if screen_y < item.row_rect.top() || screen_y >= item.row_rect.bottom() {
                            return None;
                        }
                        if read_panel {
                            let track_id = match frame.key {
                                TrackFrameKey::Alignment(track_id) => track_id,
                                TrackFrameKey::Reference
                                | TrackFrameKey::Group { .. }
                                | TrackFrameKey::Auxiliary(_) => return None,
                            };
                            return Some((
                                frame,
                                screen_y
                                    + panel
                                        .read_panel_scroll_state(track_id)
                                        .effective_scroll_points(),
                            ));
                        }
                        Some((
                            frame,
                            screen_y + self.session.panels.annotation_scroll_points,
                        ))
                    })
                    .flatten()
            })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn select_at_cursor(&mut self) {
        // Clicking an arc's end (or the coverage at a splice site) filters the Sashimi lane to
        // the junctions using that site; clicking the arc's body still selects the junction.
        if self.filter_sashimi_at_cursor() {
            return;
        }
        let scene_selection = self.alignment_scene_hit_at_cursor(self.session.panels.active().id);
        let scene_selected_chain = scene_selection.and_then(|hit| {
            let record =
                self.alignment_scene_record_for_hit(self.session.panels.active().id, hit)?;
            selected_supplementary_chain(&record, &self.session.panels.active().contig_name)
        });
        if scene_selection.is_none()
            && !self
                .session
                .panels
                .active()
                .navigation_presentation
                .is_interactive()
        {
            return;
        }
        self.clear_promoted_read();
        self.session.panels.active_mut().retained_evidence = None;
        self.session.panels.active_mut().pending_read_selection = None;
        self.session.panels.active_mut().pending_alignment_reveal = None;
        self.session.panels.active_mut().selected_junction_component = None;
        self.session.panels.active_mut().selected_feature = None;
        self.session.panels.active_mut().selected_variant = None;
        self.session.panels.active_mut().selected_gwas = None;
        self.session.panels.active_mut().selected_quantitative = None;
        self.session.panels.active_mut().selected_copy_number = None;
        self.session.panels.active_mut().selected_mutation = None;
        self.session.panels.active_mut().selected_interaction = None;
        self.session.panels.active_mut().selected_alignment_hit = None;
        if self.select_track_tile_feature_at_cursor() {
            // A feature of a lane drawn by the experimental tile pipeline.
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.feature_at_cursor() {
            self.session.panels.active_mut().selected_feature = Some(selection);
            self.toggle_gene_transcripts_on_click(selection);
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.variant_at_cursor() {
            self.session.panels.active_mut().selected_variant = Some(selection);
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.gwas_at_cursor() {
            self.session.panels.active_mut().selected_gwas = Some(selection);
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.mutation_at_cursor() {
            self.session.panels.active_mut().selected_mutation = Some(selection);
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.copy_number_at_cursor() {
            self.session.panels.active_mut().selected_copy_number = Some(selection);
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.interaction_at_cursor() {
            self.session.panels.active_mut().selected_interaction = Some(selection);
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.quantitative_at_cursor() {
            self.session.panels.active_mut().selected_quantitative = Some(selection);
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some((track_id, position)) = self.modification_position_at_cursor() {
            self.session.panels.active_mut().selected_alignment_track = Some(track_id);
            self.session
                .panels
                .active_mut()
                .selected_modification_position = Some(position);
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some((track_id, position)) = self.pileup_position_at_cursor() {
            self.session.panels.active_mut().selected_alignment_track = Some(track_id);
            self.session.panels.active_mut().selected_pileup_position = Some(position);
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some((track_id, start, end, component)) = self.junction_at_cursor() {
            self.session.panels.active_mut().selected_alignment_track = Some(track_id);
            self.session.panels.active_mut().selected_junction = Some((start, end));
            self.session.panels.active_mut().selected_junction_component = Some(component);
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_chain = None;
        } else if let Some(hit) = scene_selection {
            self.session.panels.active_mut().selected_alignment_track = Some(hit.track);
            self.session.panels.active_mut().selected_alignment_hit = Some(hit);
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = scene_selected_chain;
        } else {
            self.session.panels.active_mut().selected_alignment_track = None;
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_chain = None;
        }
        self.retain_active_read_selection_for_query_refresh();
        let cursor_y = (self.cursor.y / self.window.scale_factor()) as f32;
        let selected_track = self
            .visible_track_frame_at_screen_y(cursor_y, |_| true)
            .map(|(frame, _)| (frame.key, frame.channel));
        if let Some((key, channel)) = selected_track {
            replace_panel_track_selection(self.session.panels.active_mut(), key, channel);
        } else {
            self.session.panels.active_mut().selected_track = None;
            self.session
                .panels
                .active_mut()
                .selected_presentation_nodes
                .clear();
            self.session
                .panels
                .active_mut()
                .presentation_selection_anchor = None;
            self.session
                .panels
                .active_mut()
                .selected_alignment_tracks
                .clear();
            self.session
                .panels
                .active_mut()
                .selected_read_tracks
                .clear();
        }
        self.bump_workspace_revision();
        let granularity = self
            .session
            .panels
            .active()
            .selected_pileup_position
            .map(|_| AutomationSelectionGranularity::VisibleBin);
        self.retain_active_selection(granularity);
        let panel_id = self.session.panels.active().id;
        self.synchronize_alignment_scene_interactions(panel_id);
        self.prepare_alignment_scene_frame();
        self.mark_display_dirty("explicit display invalidation");
    }

    pub(crate) fn feature_at_cursor(&self) -> Option<FeatureSelection> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let (frame, content_y) = self.visible_track_frame_at_screen_y(y, |frame| {
            matches!(frame.kind, TrackKind::Feature | TrackKind::GeneModel)
        })?;
        let TrackFrameKey::Auxiliary(track_id) = frame.key else {
            return None;
        };
        let track = self
            .session
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)?;
        if track.collapsed {
            return None;
        }
        let display_mode = track.feature.display_mode;
        let is_pinned_gene = track.kind == TrackKind::GeneModel
            && primary_pinned_gene_id(&self.effective_auxiliary_presentation_configs())
                == Some(track_id);
        let records = self
            .session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Features(records) => Some(records.as_slice()),
                TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })?;
        let layout_maximum_rows = if is_pinned_gene {
            pinned_gene_layout_maximum_rows(frame.bottom - frame.top)
        } else {
            TrackLayoutOptions::default().maximum_rows
        };
        let object_ids = ObjectIdAllocator::new().allocate(records.len())?;
        // The same presentation the lane was drawn with, so a click lands on what is drawn.
        let presentation = feature_presentation(track, records);
        let label_widths =
            measured_feature_label_widths(&self.egui_context, records, presentation.as_ref());
        let display = layout_presented_features(
            records,
            self.session.panels.active().viewport,
            TrackLayoutOptions {
                top: frame.top,
                height: frame.bottom - frame.top,
                maximum_rows: layout_maximum_rows,
                object_id_base: object_ids.base(),
                feature_display_mode: display_mode,
                ..TrackLayoutOptions::default()
            },
            Some(&label_widths),
            presentation.as_ref(),
        )
        .0;
        feature_record_index_at_display_point(
            &display,
            records,
            x - panel_rect.left(),
            content_y,
            object_ids,
        )
        .map(|record_index| FeatureSelection {
            track_id,
            record_index,
        })
    }

    pub(crate) fn variant_at_cursor(&self) -> Option<VariantSelection> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let (frame, _) =
            self.visible_track_frame_at_screen_y(y, |frame| frame.kind == TrackKind::Variant)?;
        let TrackFrameKey::Auxiliary(track_id) = frame.key else {
            return None;
        };
        let query = self
            .session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Variants(query) => Some(query.as_ref()),
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })?;
        let local_x = f64::from(x - panel_rect.left());
        let position = self
            .session
            .panels
            .active()
            .viewport
            .genomic_at_pixel(local_x)
            .floor()
            .max(0.0) as u64;
        let tolerance = (self.session.panels.active().viewport.bases_per_pixel() * 6.0)
            .ceil()
            .max(1.0) as u64;
        query
            .records
            .iter()
            .enumerate()
            .filter_map(|(record_index, record)| {
                let distance = genomic_distance_to_interval(position, record.start, record.end);
                (distance <= tolerance).then_some((
                    distance,
                    record.end.saturating_sub(record.start),
                    record_index,
                ))
            })
            .min()
            .map(|(_, _, record_index)| VariantSelection {
                track_id,
                record_index,
            })
    }

    pub(crate) fn gwas_at_cursor(&self) -> Option<GwasSelection> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let (frame, content_y) =
            self.visible_track_frame_at_screen_y(y, |frame| frame.kind == TrackKind::Gwas)?;
        let TrackFrameKey::Auxiliary(track_id) = frame.key else {
            return None;
        };
        let track = self
            .session
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)?;
        if track.collapsed {
            return None;
        }
        let query = self
            .session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Gwas(query) => Some(query.as_ref()),
                _ => None,
            })?;
        let local_x = x - panel_rect.left();
        gwas_record_index_at_point(
            &query.records,
            self.session.panels.active().viewport,
            local_x,
            content_y,
            frame.top,
            frame.bottom - frame.top,
            &track.gwas,
        )
        .map(|record_index| GwasSelection {
            track_id,
            record_index,
        })
    }

    pub(crate) fn quantitative_at_cursor(&self) -> Option<QuantitativeSelection> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let (frame, content_y) =
            self.visible_track_frame_at_screen_y(y, |frame| frame.kind == TrackKind::Quantitative)?;
        let TrackFrameKey::Auxiliary(track_id) = frame.key else {
            return None;
        };
        let local_x = f64::from(x - panel_rect.left());
        let position = self
            .session
            .panels
            .active()
            .viewport
            .genomic_at_pixel(local_x)
            .floor()
            .max(0.0) as u64;
        let effective_tracks = self.effective_auxiliary_presentation_configs();
        let anchor = effective_tracks.iter().find(|track| track.id == track_id)?;
        let candidates = anchor.quantitative.overlay_group.map_or_else(
            || vec![anchor],
            |group| {
                effective_tracks
                    .iter()
                    .filter(|track| {
                        track.visible
                            && track.kind == TrackKind::Quantitative
                            && track.quantitative.overlay_group == Some(group)
                    })
                    .collect::<Vec<_>>()
            },
        );
        candidates
            .into_iter()
            .filter_map(|track| {
                let query = self
                    .session
                    .panels
                    .active()
                    .track_snapshots
                    .iter()
                    .find(|snapshot| snapshot.id == track.id)
                    .and_then(|snapshot| match &snapshot.data {
                        TrackSnapshotData::Quantitative(query) => Some(query.as_ref()),
                        TrackSnapshotData::Features(_)
                        | TrackSnapshotData::Variants(_)
                        | TrackSnapshotData::CopyNumber(_)
                        | TrackSnapshotData::Mutations(_)
                        | TrackSnapshotData::Interactions(_)
                        | TrackSnapshotData::Gwas(_) => None,
                    })?;
                let record_index = quantitative_record_index_at_position(&query.records, position)?;
                let distance = quantitative_track_y_at_position(
                    track,
                    &effective_tracks,
                    &self.session.panels.active().track_snapshots,
                    self.session.panels.active().viewport,
                    position,
                    frame.top,
                    frame.bottom - frame.top,
                )
                .map_or(f32::MAX, |track_y| (content_y - track_y).abs());
                Some((distance, track.id, record_index))
            })
            .min_by(|left, right| {
                left.0
                    .total_cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1))
            })
            .map(|(_, track_id, record_index)| QuantitativeSelection {
                track_id,
                record_index,
                position,
            })
    }

    pub(crate) fn mutation_at_cursor(&self) -> Option<MutationSelection> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let (frame, content_y) =
            self.visible_track_frame_at_screen_y(y, |frame| frame.kind == TrackKind::Mutation)?;
        let TrackFrameKey::Auxiliary(track_id) = frame.key else {
            return None;
        };
        let settings = self
            .session
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| &track.mutation)?;
        let query = self
            .session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Mutations(query) => Some(query.as_ref()),
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })?;
        let local_row = content_y - frame.top - MUTATION_HEADER_HEIGHT;
        if local_row < 0.0 {
            return None;
        }
        let row_height = settings.row_height.clamp(8.0, 40.0);
        let sample_index = (local_row / row_height).floor() as usize;
        if sample_index >= query.samples.len() {
            return None;
        }
        let position = self
            .session
            .panels
            .active()
            .viewport
            .genomic_at_pixel(f64::from(x - panel_rect.left()))
            .floor()
            .max(0.0) as u64;
        mutation_record_index_at_position(&query.records, sample_index, position).map(
            |record_index| MutationSelection {
                track_id,
                record_index,
            },
        )
    }

    pub(crate) fn copy_number_at_cursor(&self) -> Option<CopyNumberSelection> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let (frame, content_y) =
            self.visible_track_frame_at_screen_y(y, |frame| frame.kind == TrackKind::CopyNumber)?;
        let TrackFrameKey::Auxiliary(track_id) = frame.key else {
            return None;
        };
        let settings = self
            .session
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| &track.copy_number)?;
        let query = self
            .session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::CopyNumber(query) => Some(query.as_ref()),
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })?;
        let local_row = content_y - frame.top - COPY_NUMBER_HEADER_HEIGHT;
        if local_row < 0.0 {
            return None;
        }
        let row_height = settings.row_height.clamp(6.0, 40.0);
        let row_index = (local_row / row_height).floor() as usize;
        let order = copy_number_sample_order(
            query.samples.len(),
            &query.sample_sort_values,
            settings.sort_mode,
        );
        let sample_index = *order.get(row_index)?;
        let position = self
            .session
            .panels
            .active()
            .viewport
            .genomic_at_pixel(f64::from(x - panel_rect.left()))
            .floor()
            .max(0.0) as u64;
        query
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| {
                segment.sample_index == sample_index
                    && segment.start <= position
                    && position < segment.end
            })
            .min_by_key(|(_, segment)| segment.end.saturating_sub(segment.start))
            .map(|(segment_index, _)| CopyNumberSelection {
                track_id,
                segment_index,
            })
    }

    pub(crate) fn interaction_at_cursor(&self) -> Option<InteractionSelection> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let (frame, _) =
            self.visible_track_frame_at_screen_y(y, |frame| frame.kind == TrackKind::Interaction)?;
        let TrackFrameKey::Auxiliary(track_id) = frame.key else {
            return None;
        };
        let query = self
            .session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Interactions(query) => Some(query.as_ref()),
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Gwas(_) => None,
            })?;
        let position = self
            .session
            .panels
            .active()
            .viewport
            .genomic_at_pixel(f64::from(x - panel_rect.left()))
            .floor()
            .max(0.0) as u64;
        let tolerance = (self.session.panels.active().viewport.bases_per_pixel() * 6.0)
            .ceil()
            .max(1.0) as u64;
        interaction_record_index_at_position(
            &query.records,
            &self.session.panels.active().contig_name,
            position,
            tolerance,
        )
        .map(|record_index| InteractionSelection {
            track_id,
            record_index,
        })
    }

    pub(crate) fn feature_inspector_snapshot(&self) -> Option<FeatureInspectorSnapshot> {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.session.panels.active().selected_feature?;
        if let Some(snapshot) = self.track_tile_feature_inspector_snapshot() {
            return Some(snapshot);
        }
        self.session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Features(records) if selection.record_index < records.len() => {
                    let mut record = records[selection.record_index].clone();
                    self.enrich_builtin_feature_record(selection.track_id, &mut record);
                    Some(FeatureInspectorSnapshot {
                        track_name: snapshot.name.clone(),
                        gene_transcripts: GeneTranscriptsSummary {
                            transcripts: gene_transcript_identifiers(records, &record),
                            toggle: self.gene_transcripts_target_of(selection),
                        },
                        record,
                    })
                }
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
    }

    pub(crate) fn interaction_inspector_snapshot(&self) -> Option<InteractionInspectorSnapshot> {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.session.panels.active().selected_interaction?;
        self.session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Interactions(query)
                    if selection.record_index < query.records.len() =>
                {
                    let settings = self
                        .session
                        .tracks
                        .tracks()
                        .iter()
                        .find(|track| track.id == selection.track_id)
                        .map_or_else(InteractionTrackSettings::default, |track| {
                            track.interaction.clone()
                        });
                    Some(InteractionInspectorSnapshot {
                        track_name: snapshot.name.clone(),
                        query: Arc::clone(query),
                        record_index: selection.record_index,
                        settings,
                    })
                }
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
    }

    pub(crate) fn quantitative_inspector_snapshot(&self) -> Option<QuantitativeInspectorSnapshot> {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.session.panels.active().selected_quantitative?;
        self.session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Quantitative(query)
                    if selection.record_index < query.records.len() =>
                {
                    let settings = self
                        .session
                        .tracks
                        .tracks()
                        .iter()
                        .find(|track| track.id == selection.track_id)
                        .map_or_else(QuantitativeTrackSettings::default, |track| {
                            track.quantitative.clone()
                        });
                    Some(QuantitativeInspectorSnapshot {
                        track_name: snapshot.name.clone(),
                        query: Arc::clone(query),
                        record_index: selection.record_index,
                        selected_position: selection.position,
                        operand: settings.operand_track.map(|operand_id| {
                            let operand_name = self
                                .session
                                .tracks
                                .tracks()
                                .iter()
                                .find(|track| track.id == operand_id)
                                .map_or_else(
                                    || format!("track {} (unavailable)", operand_id.get()),
                                    |track| track.name.clone(),
                                );
                            let value = self
                                .session
                                .panels
                                .active()
                                .track_snapshots
                                .iter()
                                .find(|snapshot| snapshot.id == operand_id)
                                .and_then(|snapshot| match &snapshot.data {
                                    TrackSnapshotData::Quantitative(query) => {
                                        quantitative_record_index_at_position(
                                            &query.records,
                                            selection.position,
                                        )
                                        .and_then(|index| query.records.get(index))
                                        .map(|record| record.value)
                                        .filter(|value| value.is_finite())
                                    }
                                    TrackSnapshotData::Features(_)
                                    | TrackSnapshotData::Variants(_)
                                    | TrackSnapshotData::CopyNumber(_)
                                    | TrackSnapshotData::Mutations(_)
                                    | TrackSnapshotData::Interactions(_)
                                    | TrackSnapshotData::Gwas(_) => None,
                                });
                            QuantitativeOperandSnapshot {
                                name: operand_name,
                                value,
                            }
                        }),
                        settings,
                    })
                }
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
    }

    pub(crate) fn gwas_inspector_snapshot(&self) -> Option<GwasInspectorSnapshot> {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.session.panels.active().selected_gwas?;
        self.session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Gwas(query) if selection.record_index < query.records.len() => {
                    let settings = self
                        .session
                        .tracks
                        .tracks()
                        .iter()
                        .find(|track| track.id == selection.track_id)
                        .map_or_else(GwasTrackSettings::default, |track| track.gwas.clone());
                    Some(GwasInspectorSnapshot {
                        track_name: snapshot.name.clone(),
                        query: Arc::clone(query),
                        record_index: selection.record_index,
                        settings,
                    })
                }
                _ => None,
            })
    }

    pub(crate) fn copy_number_inspector_snapshot(&self) -> Option<CopyNumberInspectorSnapshot> {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.session.panels.active().selected_copy_number?;
        self.session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::CopyNumber(query)
                    if selection.segment_index < query.segments.len() =>
                {
                    let settings = self
                        .session
                        .tracks
                        .tracks()
                        .iter()
                        .find(|track| track.id == selection.track_id)
                        .map_or_else(CopyNumberTrackSettings::default, |track| {
                            track.copy_number.clone()
                        });
                    Some(CopyNumberInspectorSnapshot {
                        track_name: snapshot.name.clone(),
                        query: Arc::clone(query),
                        segment_index: selection.segment_index,
                        settings,
                    })
                }
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
    }

    pub(crate) fn mutation_inspector_snapshot(&self) -> Option<MutationInspectorSnapshot> {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.session.panels.active().selected_mutation?;
        self.session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Mutations(query)
                    if selection.record_index < query.records.len() =>
                {
                    Some(MutationInspectorSnapshot {
                        track_name: snapshot.name.clone(),
                        contig: self.session.panels.active().contig_name.clone(),
                        query: Arc::clone(query),
                        record_index: selection.record_index,
                    })
                }
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
    }

    pub(crate) fn variant_inspector_snapshot(&self) -> Option<VariantInspectorSnapshot> {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.session.panels.active().selected_variant?;
        self.session
            .panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Variants(query)
                    if selection.record_index < query.records.len() =>
                {
                    let settings = self
                        .session
                        .tracks
                        .tracks()
                        .iter()
                        .find(|track| track.id == selection.track_id)
                        .map_or_else(VariantTrackSettings::default, |track| track.variant.clone());
                    Some(VariantInspectorSnapshot {
                        track_name: snapshot.name.clone(),
                        query: Arc::clone(query),
                        record_index: selection.record_index,
                        settings,
                    })
                }
                TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Features(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
    }

    pub(crate) fn variant_alignment_evidence(
        &self,
        inspector: &VariantInspectorSnapshot,
    ) -> Vec<VariantAlignmentEvidence> {
        let record = &inspector.query.records[inspector.record_index];
        self.session
            .alignment_tracks
            .iter()
            .filter_map(|track| {
                if let Some(presented) = self
                    .presented_alignment_lane(self.session.panels.active().id, track.config.id)
                    .filter(|presented| presented.lane.summary_current())
                    && let Some(scene) = presented.lane.snapshot.as_ref()
                {
                    let site = scene.pileup_site_at(record.start)?;
                    let interval = GenomicInterval::new(
                        scene.prepared_interval().contig(),
                        record.start,
                        record.start.checked_add(1)?,
                    )
                    .ok()?;
                    let (modification_series, modification_calls) = scene
                        .summaries()
                        .modification_summary_at(record.start)
                        .and_then(|(summary_interval, summary)| {
                            let index = summary.bin_index_at(summary_interval, record.start)?;
                            Some(summary.series.iter().fold(
                                (0_usize, 0_u32),
                                |(series_count, calls), series| {
                                    let modified =
                                        series.bins.get(index).map_or(0, |bin| bin.modified_calls);
                                    (
                                        series_count.saturating_add(usize::from(modified > 0)),
                                        calls.saturating_add(modified),
                                    )
                                },
                            ))
                        })
                        .unwrap_or_default();
                    let pair_templates_sampled = scene
                        .summaries()
                        .pair_summary(scene.prepared_interval())
                        .ok()
                        .map_or(0, |summary| summary.sampled_templates);
                    return Some(VariantAlignmentEvidence {
                        track_id: track.config.id,
                        track_name: track.config.name.clone(),
                        position: record.start,
                        bin_start: interval.start(),
                        bin_end: interval.end(),
                        depth: site.evidence.bin.depth,
                        forward_depth: site.evidence.bin.forward_depth,
                        reverse_depth: site.evidence.bin.reverse_depth,
                        allele_summary: variant_allele_evidence_summary(
                            record,
                            interval.len(),
                            &site.evidence.bin,
                        ),
                        modification_series,
                        modification_calls,
                        pair_templates_sampled,
                        retained_records: scene.reads().evidence().record_count(),
                        complete_individual_reads: matches!(
                            presented.source.individual_reads,
                            AlignmentIndividualReadCapability::Complete { .. }
                        ),
                        complete_pileup: true,
                    });
                }
                None
            })
            .collect()
    }

    pub(crate) fn select_variant_alignment_evidence(
        &mut self,
        track_id: TrackId,
        position: u64,
        modification: bool,
    ) {
        self.session.panels.active_mut().selected_alignment_track = Some(track_id);
        replace_panel_track_selection(
            self.session.panels.active_mut(),
            TrackFrameKey::Alignment(track_id),
            ui::TrackPane::Annotation,
        );
        self.session.panels.active_mut().pending_read_selection = None;
        self.session.panels.active_mut().pending_alignment_reveal = None;
        self.session.panels.active_mut().selected_alignment_hit = None;
        self.session.panels.active_mut().selected_junction = None;
        self.session.panels.active_mut().selected_junction_component = None;
        self.session.panels.active_mut().selected_chain = None;
        self.session.panels.active_mut().selected_pileup_position =
            (!modification).then_some(position);
        self.session
            .panels
            .active_mut()
            .selected_modification_position = modification.then_some(position);
        self.rebuild_genomic_display(self.session.panels.active().id);
        self.mark_display_dirty("explicit display invalidation");
    }

    pub(crate) fn modification_position_at_cursor(&self) -> Option<(TrackId, u64)> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let track_id = alignment_lane_in_pane_layout(
            &self.session.panels.active().pane_layout.annotation,
            "Modifications",
            y,
        )?;
        if let Some(scene) = self
            .presented_alignment_lane(self.session.panels.active().id, track_id)
            .and_then(|presented| presented.lane.snapshot.as_ref())
        {
            let local_x = f64::from(x - panel_rect.left());
            let position = self
                .session
                .panels
                .active()
                .viewport
                .genomic_at_pixel(local_x)
                .floor() as u64;
            let interval = GenomicInterval::new(
                scene.prepared_interval().contig(),
                position,
                position.checked_add(1)?,
            )
            .ok()?;
            // The lane's bin under the cursor at this zoom (one base when zoomed in).
            let bin_bases = rgv_alignment_scene::modification_lane_bin_bases(
                self.session.panels.active().viewport.bases_per_pixel(),
            );
            let present = scene
                .summary_pages_for(SummaryPageFamily::Modifications, interval)
                .iter()
                .any(|page| {
                    let page_start = page.interval().start();
                    let bin_start =
                        page_start.saturating_add((position - page_start) / bin_bases * bin_bases);
                    let bin_end = bin_start.saturating_add(bin_bases);
                    page.primitives().iter().any(|primitive| {
                        let start = page_start.saturating_add(primitive.start_offset);
                        let end = page_start.saturating_add(primitive.end_offset);
                        start < bin_end
                            && bin_start < end
                            && matches!(
                                primitive.component,
                                SummarySemanticComponent::Modification { .. }
                            )
                    })
                });
            return present.then_some((track_id, position));
        }
        None
    }

    pub(crate) fn pileup_position_at_cursor(&self) -> Option<(TrackId, u64)> {
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return None;
        }
        let track_id = alignment_lane_in_pane_layout(
            &self.session.panels.active().pane_layout.annotation,
            "Coverage",
            y,
        )?;
        if let Some(scene) = self
            .presented_alignment_lane(self.session.panels.active().id, track_id)
            .and_then(|presented| presented.lane.snapshot.as_ref())
        {
            let position = self
                .session
                .panels
                .active()
                .viewport
                .genomic_at_pixel(f64::from(x - panel_rect.left()))
                .floor() as u64;
            return scene
                .prepared_interval()
                .contains(position)
                .then_some((track_id, position));
        }
        None
    }

    pub(crate) fn junction_at_cursor(&self) -> Option<(TrackId, u64, u64, JunctionArcComponent)> {
        self.junction_arc_hover_at_cursor().map(|hover| {
            (
                hover.key.track_id,
                hover.key.start,
                hover.key.end,
                hover.key.component,
            )
        })
    }

    pub(crate) fn junction_arc_hover_at_cursor(&self) -> Option<JunctionArcHover> {
        self.junction_arc_hit_test_at_cursor().hover
    }

    pub(crate) fn junction_arc_hit_test_at_cursor(&self) -> JunctionArcHitTestResult {
        let panel = self.session.panels.active();
        if !panel.navigation_presentation.is_interactive() {
            return JunctionArcHitTestResult {
                gate: SashimiHitTestGate::PresentationUnsettled,
                ..JunctionArcHitTestResult::default()
            };
        }
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if x < panel_rect.left() || x >= panel_rect.right() {
            return JunctionArcHitTestResult {
                gate: SashimiHitTestGate::OutsidePanel,
                ..JunctionArcHitTestResult::default()
            };
        }
        let Some(track_id) =
            alignment_lane_in_pane_layout(&panel.pane_layout.annotation, "Sashimi", y)
        else {
            return JunctionArcHitTestResult {
                gate: SashimiHitTestGate::OutsideSashimiLane,
                ..JunctionArcHitTestResult::default()
            };
        };
        if let Some(result) = self.native_junction_arc_hit_test_at_point(
            panel.id,
            track_id,
            f64::from(x),
            f64::from(y),
        ) {
            return result;
        }
        JunctionArcHitTestResult {
            gate: SashimiHitTestGate::SceneUnavailable,
            lane_track_id: Some(track_id),
            ..JunctionArcHitTestResult::default()
        }
    }

    /// Returns `Some` whenever the published native summary frame owns this lane, including when
    /// no arc is under the pointer. That authoritative miss must not fall through to stale legacy
    /// display-list geometry.
    fn native_junction_arc_hit_test_at_point(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        x: f64,
        y: f64,
    ) -> Option<JunctionArcHitTestResult> {
        let epoch = self.alignment_presented_epoch.as_ref()?;
        let bundle = epoch.gpu_bundle.as_ref()?;
        let panel_frame = epoch.panel_frames.get(&panel_id)?;
        let summaries = bundle.summaries.as_ref()?;
        if !alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
            || panel_frame.camera_revision != bundle.target_revision
            || summaries
                .camera_sequences
                .iter()
                .any(|sequence| *sequence != bundle.publication_attempt_sequence)
            || !summaries.has_junction_lane(panel_id.get(), track_id.get())
        {
            return None;
        }
        let content_revision = self
            .session
            .panels
            .get(panel_id)?
            .navigation_presentation
            .committed_content_revision;
        let hit = self.alignment_summary_gpu_renderer.hit_test_junction_lane(
            summaries,
            panel_id.get(),
            track_id.get(),
            x,
            y,
            f64::from(JUNCTION_ARC_MINIMUM_ACQUISITION_RADIUS),
        );
        let hover = hit.and_then(|hit| {
            if hit.coherence_scope != panel_id.get() || hit.lane_identity != track_id.get() {
                return None;
            }
            let component = layout_junction_component(hit.component)?;
            let key = JunctionArcKey {
                track_id,
                start: hit.evidence.start,
                end: hit.evidence.end,
                component,
            };
            Some(JunctionArcHover {
                key,
                interaction_token: native_junction_interaction_token(key),
                distance_points: hit.distance as f32,
                content_revision,
                splice_site: crate::workspace::alignment_management::sashimi_presentation::splice_site_at_arc_end(
                    hit.bounds.x,
                    hit.bounds.width,
                    x,
                    hit.evidence.start,
                    hit.evidence.end,
                ),
                evidence: hit.evidence,
                presentation: hit.presentation,
            })
        });
        Some(JunctionArcHitTestResult {
            hover,
            native_authority: true,
            gate: SashimiHitTestGate::Tested,
            lane_track_id: Some(track_id),
            total_targets: usize::from(summaries.has_junction_lane(panel_id.get(), track_id.get())),
            track_targets: usize::from(hover.is_some()),
            current_revision_targets: usize::from(hover.is_some()),
            bounds_candidates: usize::from(hover.is_some()),
            curve_evaluations: usize::from(hover.is_some()),
            nearest_curve_distance: hover.map(|hover| hover.distance_points),
            nearest_acquisition_radius: Some(JUNCTION_ARC_MINIMUM_ACQUISITION_RADIUS),
            ..JunctionArcHitTestResult::default()
        })
    }

    pub(crate) fn native_junction_lane_is_present(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> bool {
        let Some(epoch) = self.alignment_presented_epoch.as_ref() else {
            return false;
        };
        let Some(bundle) = epoch.gpu_bundle.as_ref() else {
            return false;
        };
        let Some(panel_frame) = epoch.panel_frames.get(&panel_id) else {
            return false;
        };
        let Some(summaries) = bundle.summaries.as_ref() else {
            return false;
        };
        alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
            && panel_frame.camera_revision == bundle.target_revision
            && summaries
                .camera_sequences
                .iter()
                .all(|sequence| *sequence == bundle.publication_attempt_sequence)
            && summaries.has_junction_lane(panel_id.get(), track_id.get())
    }

    pub(crate) fn alignment_scene_hit_at_cursor(&self, panel_id: PanelId) -> Option<AlignmentHit> {
        let scale = self.window.scale_factor().max(f64::EPSILON);
        let x = self.cursor.x / scale;
        let y = self.cursor.y / scale;
        let epoch = self.alignment_presented_epoch.as_ref()?;
        let frame = epoch.panel_frames.get(&panel_id)?;
        let bundle = epoch.gpu_bundle.as_ref()?;
        let reads = bundle.reads.as_ref()?;
        if !alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
            || frame.camera_revision != bundle.target_revision
            || reads
                .camera_sequences
                .iter()
                .any(|sequence| *sequence != bundle.publication_attempt_sequence)
        {
            return None;
        }
        frame
            .lanes
            .iter()
            .filter_map(|lane| {
                lane.read_camera?;
                let scene = lane.snapshot.as_ref()?.reads();
                let source = bundle.lane_authorities.iter().find(|authority| {
                    authority.panel_id == panel_id
                        && authority.track_id == lane.track_id
                        && authority.target_revision == bundle.target_revision
                        && authority.source_camera == lane.preparation_camera
                })?;
                let authority = bundle.read_authorities.iter().find(|authority| {
                    authority.panel_id == panel_id
                        && authority.track_id == lane.track_id
                        && authority.target_revision == bundle.target_revision
                        && authority.source_request_token == source.source_request_token
                        && authority.desired_request_token == source.desired_request_token
                        && authority.source_camera == lane.preparation_camera
                        && authority.scene_revision == scene.revision()
                })?;
                let clip = authority.clip;
                if x < clip.x || x >= clip.x + clip.width || y < clip.y || y >= clip.y + clip.height
                {
                    return None;
                }
                CpuAlignmentProjector::pick_panel_lane(lane, x, y)
                    .ok()
                    .flatten()
            })
            .next()
    }

    pub(crate) fn select_visible_read(&mut self, forward: bool) {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return;
        }
        self.clear_promoted_read();
        self.session.panels.active_mut().pending_read_selection = None;
        self.session.panels.active_mut().pending_alignment_reveal = None;
        let panel_id = self.session.panels.active().id;
        let track_id = self
            .session
            .panels
            .active()
            .selected_alignment_hit
            .map(|hit| hit.track)
            .or(self.session.panels.active().selected_alignment_track)
            .unwrap_or_else(|| self.primary_alignment_track_id());
        let native_scene = self
            .presented_alignment_lane(panel_id, track_id)
            .and_then(|presented| presented.lane.snapshot.as_ref())
            .cloned();
        if let Some(snapshot) = native_scene {
            let scene = snapshot.reads();
            let direction = if forward {
                AlignmentTraversalDirection::Next
            } else {
                AlignmentTraversalDirection::Previous
            };
            let current = self
                .session
                .panels
                .active()
                .selected_alignment_hit
                .filter(|hit| hit.track == track_id && scene.contains_handle(hit.handle))
                .map(|hit| hit.handle);
            let handle = current
                .and_then(|handle| scene.adjacent_handle(handle, direction))
                .or_else(|| scene.edge_handle(direction));
            let Some(handle) = handle else {
                return;
            };
            let selected_chain = scene
                .inspect_retained(handle)
                .ok()
                .flatten()
                .and_then(|record| {
                    selected_supplementary_chain(&record, &self.session.panels.active().contig_name)
                });
            self.session.panels.active_mut().selected_alignment_track = Some(track_id);
            self.session.panels.active_mut().selected_alignment_hit = Some(AlignmentHit {
                track: track_id,
                scene_revision: scene.revision(),
                handle,
                component: AlignmentComponent::ReadSpan,
            });
            self.session.panels.active_mut().selected_pileup_position = None;
            self.session
                .panels
                .active_mut()
                .selected_modification_position = None;
            self.session.panels.active_mut().selected_junction = None;
            self.session.panels.active_mut().selected_junction_component = None;
            self.session.panels.active_mut().selected_chain = selected_chain;
            replace_panel_track_selection(
                self.session.panels.active_mut(),
                TrackFrameKey::Alignment(track_id),
                ui::TrackPane::Reads,
            );
            self.synchronize_alignment_scene_interactions(panel_id);
            self.prepare_alignment_scene_frame();
            self.mark_display_dirty("explicit display invalidation");
            return;
        }
    }

    pub(crate) fn clear_semantic_evidence_selection(&mut self) {
        self.session.panels.active_mut().pending_read_selection = None;
        self.session.panels.active_mut().pending_alignment_reveal = None;
        self.session.panels.active_mut().selected_alignment_hit = None;
        self.session.panels.active_mut().selected_alignment_track = None;
        self.session.panels.active_mut().selected_pileup_position = None;
        self.session
            .panels
            .active_mut()
            .selected_modification_position = None;
        self.session.panels.active_mut().selected_junction = None;
        self.session.panels.active_mut().selected_junction_component = None;
        self.session.panels.active_mut().selected_feature = None;
        self.session.panels.active_mut().selected_variant = None;
        self.session.panels.active_mut().selected_gwas = None;
        self.session.panels.active_mut().selected_quantitative = None;
        self.session.panels.active_mut().selected_copy_number = None;
        self.session.panels.active_mut().selected_mutation = None;
        self.session.panels.active_mut().selected_interaction = None;
        self.session.panels.active_mut().selected_chain = None;
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn select_loaded_track_evidence(&mut self, forward: bool) {
        if !self
            .session
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return;
        }
        let Some(track_key) = self.session.panels.active().selected_track else {
            "Select a track before moving through its evidence"
                .clone_into(&mut self.session.data_status);
            return;
        };
        match track_key {
            TrackFrameKey::Group { channel, id } => {
                self.session.data_status =
                    format!("{channel:?} track group {id} has no discrete evidence records");
            }
            TrackFrameKey::Reference => {
                "The reference track has no discrete evidence records"
                    .clone_into(&mut self.session.data_status);
            }
            TrackFrameKey::Alignment(track_id) => {
                let panel_id = self.session.panels.active().id;
                let native_scene = self
                    .presented_alignment_lane(panel_id, track_id)
                    .and_then(|presented| presented.lane.snapshot.as_ref())
                    .cloned();
                if let Some(snapshot) = native_scene {
                    let scene = snapshot.reads();
                    let direction = if forward {
                        AlignmentTraversalDirection::Next
                    } else {
                        AlignmentTraversalDirection::Previous
                    };
                    let current = self
                        .session
                        .panels
                        .active()
                        .selected_alignment_hit
                        .filter(|hit| hit.track == track_id && scene.contains_handle(hit.handle))
                        .map(|hit| hit.handle);
                    let handle = current
                        .and_then(|handle| scene.adjacent_handle(handle, direction))
                        .or_else(|| scene.edge_handle(direction));
                    let Some(handle) = handle else {
                        "The selected alignment track has no complete reads available"
                            .clone_into(&mut self.session.data_status);
                        return;
                    };
                    let selected_chain =
                        scene
                            .inspect_retained(handle)
                            .ok()
                            .flatten()
                            .and_then(|record| {
                                selected_supplementary_chain(
                                    &record,
                                    &self.session.panels.active().contig_name,
                                )
                            });
                    self.clear_promoted_read();
                    self.clear_semantic_evidence_selection();
                    self.session.panels.active_mut().selected_alignment_track = Some(track_id);
                    self.session.panels.active_mut().selected_alignment_hit = Some(AlignmentHit {
                        track: track_id,
                        scene_revision: scene.revision(),
                        handle,
                        component: AlignmentComponent::ReadSpan,
                    });
                    self.session.panels.active_mut().selected_chain = selected_chain;
                    replace_panel_track_selection(
                        self.session.panels.active_mut(),
                        TrackFrameKey::Alignment(track_id),
                        ui::TrackPane::Reads,
                    );
                    self.session.data_status =
                        format!("Selected complete scene read in track {}", track_id.get());
                    self.synchronize_alignment_scene_interactions(panel_id);
                    self.prepare_alignment_scene_frame();
                    self.mark_display_dirty("explicit display invalidation");
                    return;
                }
                "The selected alignment track has no committed complete scene"
                    .clone_into(&mut self.session.data_status);
            }
            TrackFrameKey::Auxiliary(track_id) => {
                let Some(snapshot) = self
                    .session
                    .panels
                    .active()
                    .track_snapshots
                    .iter()
                    .find(|snapshot| snapshot.id == track_id)
                    .cloned()
                else {
                    "The selected track has no loaded evidence"
                        .clone_into(&mut self.session.data_status);
                    return;
                };
                let selection = match snapshot.data {
                    TrackSnapshotData::Features(records) => {
                        let current = self
                            .session
                            .panels
                            .active()
                            .selected_feature
                            .filter(|selection| selection.track_id == track_id)
                            .map(|selection| selection.record_index);
                        next_evidence_index(current, records.len(), forward)
                            .map(EvidenceSelection::Feature)
                    }
                    TrackSnapshotData::Variants(query) => {
                        let current = self
                            .session
                            .panels
                            .active()
                            .selected_variant
                            .filter(|selection| selection.track_id == track_id)
                            .map(|selection| selection.record_index);
                        next_evidence_index(current, query.records.len(), forward)
                            .map(EvidenceSelection::Variant)
                    }
                    TrackSnapshotData::Gwas(query) => {
                        let current = self
                            .session
                            .panels
                            .active()
                            .selected_gwas
                            .filter(|selection| selection.track_id == track_id)
                            .map(|selection| selection.record_index);
                        next_evidence_index(current, query.records.len(), forward)
                            .map(EvidenceSelection::Gwas)
                    }
                    TrackSnapshotData::Quantitative(query) => {
                        let current = self
                            .session
                            .panels
                            .active()
                            .selected_quantitative
                            .filter(|selection| selection.track_id == track_id)
                            .map(|selection| selection.record_index);
                        next_evidence_index(current, query.records.len(), forward).map(|index| {
                            EvidenceSelection::Quantitative(index, query.records[index].start)
                        })
                    }
                    TrackSnapshotData::CopyNumber(query) => {
                        let current = self
                            .session
                            .panels
                            .active()
                            .selected_copy_number
                            .filter(|selection| selection.track_id == track_id)
                            .map(|selection| selection.segment_index);
                        next_evidence_index(current, query.segments.len(), forward)
                            .map(EvidenceSelection::CopyNumber)
                    }
                    TrackSnapshotData::Mutations(query) => {
                        let current = self
                            .session
                            .panels
                            .active()
                            .selected_mutation
                            .filter(|selection| selection.track_id == track_id)
                            .map(|selection| selection.record_index);
                        next_evidence_index(current, query.records.len(), forward)
                            .map(EvidenceSelection::Mutation)
                    }
                    TrackSnapshotData::Interactions(query) => {
                        let current = self
                            .session
                            .panels
                            .active()
                            .selected_interaction
                            .filter(|selection| selection.track_id == track_id)
                            .map(|selection| selection.record_index);
                        next_evidence_index(current, query.records.len(), forward)
                            .map(EvidenceSelection::Interaction)
                    }
                };
                let Some(selection) = selection else {
                    "The selected track has no loaded evidence"
                        .clone_into(&mut self.session.data_status);
                    return;
                };
                self.clear_semantic_evidence_selection();
                let (kind, index) = match selection {
                    EvidenceSelection::Feature(index) => {
                        self.session.panels.active_mut().selected_feature =
                            Some(FeatureSelection {
                                track_id,
                                record_index: index,
                            });
                        ("feature", index)
                    }
                    EvidenceSelection::Variant(index) => {
                        self.session.panels.active_mut().selected_variant =
                            Some(VariantSelection {
                                track_id,
                                record_index: index,
                            });
                        ("variant", index)
                    }
                    EvidenceSelection::Gwas(index) => {
                        self.session.panels.active_mut().selected_gwas = Some(GwasSelection {
                            track_id,
                            record_index: index,
                        });
                        ("GWAS association", index)
                    }
                    EvidenceSelection::Quantitative(index, position) => {
                        self.session.panels.active_mut().selected_quantitative =
                            Some(QuantitativeSelection {
                                track_id,
                                record_index: index,
                                position,
                            });
                        ("quantitative record", index)
                    }
                    EvidenceSelection::CopyNumber(index) => {
                        self.session.panels.active_mut().selected_copy_number =
                            Some(CopyNumberSelection {
                                track_id,
                                segment_index: index,
                            });
                        ("copy-number segment", index)
                    }
                    EvidenceSelection::Mutation(index) => {
                        self.session.panels.active_mut().selected_mutation =
                            Some(MutationSelection {
                                track_id,
                                record_index: index,
                            });
                        ("mutation", index)
                    }
                    EvidenceSelection::Interaction(index) => {
                        self.session.panels.active_mut().selected_interaction =
                            Some(InteractionSelection {
                                track_id,
                                record_index: index,
                            });
                        ("interaction", index)
                    }
                };
                self.session.data_status = format!(
                    "Selected loaded {kind} {} in track {}",
                    index + 1,
                    track_id.get()
                );
            }
        }
        self.retain_active_read_selection_for_query_refresh();
        self.mark_display_dirty("explicit display invalidation");
    }

    pub(crate) fn promote_selected_read_to_top(&mut self) -> bool {
        let panel = self.session.panels.active();
        let Some(track_id) = panel
            .selected_alignment_hit
            .map(|hit| hit.track)
            .or(panel.selected_alignment_track)
        else {
            return false;
        };
        let read_name = panel
            .selected_alignment_hit
            .and_then(|hit| self.alignment_scene_record_for_hit(panel.id, hit))
            .map(|record| record.read_name);
        let Some(read_name) = read_name.filter(|name| !name.is_empty()) else {
            return false;
        };
        let promoted = (track_id, alignment_read_name_hash(&read_name));
        if self.promoted_read == Some(promoted) {
            return true;
        }
        self.promoted_read = Some(promoted);
        self.invalidate_alignment_row_layouts();
        true
    }

    pub(crate) fn select_read_or_template_by_name(
        &mut self,
        track_id: TrackId,
        name: &[u8],
    ) -> bool {
        if name.is_empty() {
            return false;
        }
        let panel_id = self.session.panels.active().id;
        let native_scene = self
            .presented_alignment_lane(panel_id, track_id)
            .and_then(|presented| presented.lane.snapshot.as_ref())
            .cloned();
        let Some(snapshot) = native_scene else {
            return false;
        };
        let scene = snapshot.reads();
        let Some(handle) = scene.handles_for_read_name(name).first().copied() else {
            return false;
        };
        let chain = scene
            .inspect_retained(handle)
            .ok()
            .flatten()
            .and_then(|record| {
                selected_supplementary_chain(&record, &self.session.panels.active().contig_name)
            });
        self.clear_promoted_read();
        self.session.panels.active_mut().pending_read_selection = None;
        self.session.panels.active_mut().pending_alignment_reveal = None;
        self.session.panels.active_mut().selected_alignment_hit = Some(AlignmentHit {
            track: track_id,
            scene_revision: scene.revision(),
            handle,
            component: AlignmentComponent::ReadSpan,
        });
        self.session.panels.active_mut().selected_alignment_track = Some(track_id);
        replace_panel_track_selection(
            self.session.panels.active_mut(),
            TrackFrameKey::Alignment(track_id),
            ui::TrackPane::Reads,
        );
        self.session.panels.active_mut().selected_pileup_position = None;
        self.session
            .panels
            .active_mut()
            .selected_modification_position = None;
        self.session.panels.active_mut().selected_junction = None;
        self.session.panels.active_mut().selected_junction_component = None;
        self.session.panels.active_mut().selected_chain = chain;
        self.synchronize_alignment_scene_interactions(panel_id);
        self.prepare_alignment_scene_frame();
        self.mark_display_dirty("explicit display invalidation");
        true
    }

    pub(crate) fn clear_promoted_read(&mut self) {
        if self.promoted_read.take().is_some() {
            self.invalidate_alignment_row_layouts();
        }
    }

    pub(crate) fn invalidate_alignment_row_layouts(&mut self) {
        for track in &mut self.session.alignment_tracks {
            if track.scene_engine.is_none() {
                continue;
            }
            for projection in track.projections.values_mut() {
                projection.advance_scene_generation();
            }
        }
        self.request_all_panel_queries();
        self.mark_display_dirty("explicit display invalidation");
    }
}
