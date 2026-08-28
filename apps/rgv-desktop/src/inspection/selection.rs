use super::super::*;
use crate::display::primary_pinned_gene_id;

impl Viewer {
    fn visible_track_frame_at_screen_y(
        &self,
        screen_y: f32,
        predicate: impl Fn(&TrackFrame) -> bool,
    ) -> Option<(&TrackFrame, f32)> {
        let panel = self.panels.active();
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
                        Some((frame, screen_y + self.panels.annotation_scroll_points))
                    })
                    .flatten()
            })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn select_at_cursor(&mut self) {
        if !self
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return;
        }
        self.clear_promoted_read();
        self.panels.active_mut().retained_evidence = None;
        self.panels.active_mut().pending_read_selection = None;
        self.panels.active_mut().selected_junction_component = None;
        self.panels.active_mut().selected_feature = None;
        self.panels.active_mut().selected_variant = None;
        self.panels.active_mut().selected_gwas = None;
        self.panels.active_mut().selected_quantitative = None;
        self.panels.active_mut().selected_copy_number = None;
        self.panels.active_mut().selected_mutation = None;
        self.panels.active_mut().selected_interaction = None;
        if let Some(selection) = self.feature_at_cursor() {
            self.panels.active_mut().selected_feature = Some(selection);
            self.panels.active_mut().selected_alignment_track = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.variant_at_cursor() {
            self.panels.active_mut().selected_variant = Some(selection);
            self.panels.active_mut().selected_alignment_track = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.gwas_at_cursor() {
            self.panels.active_mut().selected_gwas = Some(selection);
            self.panels.active_mut().selected_alignment_track = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.mutation_at_cursor() {
            self.panels.active_mut().selected_mutation = Some(selection);
            self.panels.active_mut().selected_alignment_track = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.copy_number_at_cursor() {
            self.panels.active_mut().selected_copy_number = Some(selection);
            self.panels.active_mut().selected_alignment_track = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.interaction_at_cursor() {
            self.panels.active_mut().selected_interaction = Some(selection);
            self.panels.active_mut().selected_alignment_track = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some(selection) = self.quantitative_at_cursor() {
            self.panels.active_mut().selected_quantitative = Some(selection);
            self.panels.active_mut().selected_alignment_track = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some((track_id, position)) = self.modification_position_at_cursor() {
            self.panels.active_mut().selected_alignment_track = Some(track_id);
            self.panels.active_mut().selected_modification_position = Some(position);
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some((track_id, position)) = self.pileup_position_at_cursor() {
            self.panels.active_mut().selected_alignment_track = Some(track_id);
            self.panels.active_mut().selected_pileup_position = Some(position);
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_chain = None;
        } else if let Some((track_id, start, end, component)) = self.junction_at_cursor() {
            self.panels.active_mut().selected_alignment_track = Some(track_id);
            self.panels.active_mut().selected_junction = Some((start, end));
            self.panels.active_mut().selected_junction_component = Some(component);
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_record = None;
            self.panels.active_mut().selected_read_event = None;
            self.panels.active_mut().selected_chain = None;
        } else {
            let selected_record = self.record_at_cursor();
            let selected_chain = selected_record.and_then(|(track_id, index)| {
                let record = self
                    .complete_read_query_for_track(track_id)?
                    .materialize_record(index)
                    .ok()?;
                selected_supplementary_chain(&record, &self.panels.active().contig_name)
            });
            self.panels.active_mut().selected_alignment_track =
                selected_record.map(|(track_id, _)| track_id);
            self.panels.active_mut().selected_record = selected_record.map(|(_, index)| index);
            self.panels.active_mut().selected_read_event =
                selected_record.and_then(|(track_id, record_index)| {
                    self.read_event_at_cursor(track_id, record_index)
                });
            self.panels.active_mut().selected_pileup_position = None;
            self.panels.active_mut().selected_modification_position = None;
            self.panels.active_mut().selected_junction = None;
            self.panels.active_mut().selected_chain = selected_chain;
        }
        self.retain_active_read_selection_for_query_refresh();
        let cursor_y = (self.cursor.y / self.window.scale_factor()) as f32;
        let selected_track = self
            .visible_track_frame_at_screen_y(cursor_y, |_| true)
            .map(|(frame, _)| (frame.key, frame.channel));
        if let Some((key, channel)) = selected_track {
            replace_panel_track_selection(self.panels.active_mut(), key, channel);
        } else {
            self.panels.active_mut().selected_track = None;
            self.panels.active_mut().selected_presentation_nodes.clear();
            self.panels.active_mut().presentation_selection_anchor = None;
            self.panels.active_mut().selected_alignment_tracks.clear();
            self.panels.active_mut().selected_read_tracks.clear();
        }
        self.bump_workspace_revision();
        let granularity = self
            .panels
            .active()
            .selected_pileup_position
            .map(|_| AutomationSelectionGranularity::VisibleBin);
        self.retain_active_selection(granularity);
        self.display_dirty = true;
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
        let position = self
            .panels
            .active()
            .viewport
            .genomic_at_pixel(f64::from(x - panel_rect.left()))
            .floor()
            .max(0.0) as u64;
        let layout_maximum_rows = if is_pinned_gene {
            pinned_gene_layout_maximum_rows(frame.bottom - frame.top)
        } else {
            TrackLayoutOptions::default().maximum_rows
        };
        let maximum_rows = feature_layout_row_count(layout_maximum_rows, display_mode);
        let row_height = (frame.bottom - frame.top) / maximum_rows as f32;
        let selected_row =
            ((content_y - frame.top) / row_height.max(f32::EPSILON)).floor() as usize;
        feature_record_index_at_position_and_row(
            records,
            self.panels.active().viewport,
            position,
            selected_row,
            maximum_rows,
            display_mode,
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
            .panels
            .active()
            .viewport
            .genomic_at_pixel(local_x)
            .floor()
            .max(0.0) as u64;
        let tolerance = (self.panels.active().viewport.bases_per_pixel() * 6.0)
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
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)?;
        if track.collapsed {
            return None;
        }
        let query = self
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
            self.panels.active().viewport,
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
                    &self.panels.active().track_snapshots,
                    self.panels.active().viewport,
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
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| &track.mutation)?;
        let query = self
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
            .tracks
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| &track.copy_number)?;
        let query = self
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
            .panels
            .active()
            .viewport
            .genomic_at_pixel(f64::from(x - panel_rect.left()))
            .floor()
            .max(0.0) as u64;
        let tolerance = (self.panels.active().viewport.bases_per_pixel() * 6.0)
            .ceil()
            .max(1.0) as u64;
        interaction_record_index_at_position(
            &query.records,
            &self.panels.active().contig_name,
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
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.panels.active().selected_feature?;
        self.panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Features(records) if selection.record_index < records.len() => {
                    Some(FeatureInspectorSnapshot {
                        track_name: snapshot.name.clone(),
                        record: records[selection.record_index].clone(),
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
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.panels.active().selected_interaction?;
        self.panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Interactions(query)
                    if selection.record_index < query.records.len() =>
                {
                    let settings = self
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
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.panels.active().selected_quantitative?;
        self.panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Quantitative(query)
                    if selection.record_index < query.records.len() =>
                {
                    let settings = self
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
                                .tracks
                                .tracks()
                                .iter()
                                .find(|track| track.id == operand_id)
                                .map_or_else(
                                    || format!("track {} (unavailable)", operand_id.get()),
                                    |track| track.name.clone(),
                                );
                            let value = self
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
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.panels.active().selected_gwas?;
        self.panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Gwas(query) if selection.record_index < query.records.len() => {
                    let settings = self
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
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.panels.active().selected_copy_number?;
        self.panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::CopyNumber(query)
                    if selection.segment_index < query.segments.len() =>
                {
                    let settings = self
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
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.panels.active().selected_mutation?;
        self.panels
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
                        contig: self.panels.active().contig_name.clone(),
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
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let selection = self.panels.active().selected_variant?;
        self.panels
            .active()
            .track_snapshots
            .iter()
            .find(|snapshot| snapshot.id == selection.track_id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::Variants(query)
                    if selection.record_index < query.records.len() =>
                {
                    let settings = self
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
        self.alignment_tracks
            .iter()
            .filter_map(|track| {
                let query = self.alignment_query_for_track(track.config.id)?;
                let (bin_index, bin) = query.pileup.bin_at(query.interval, record.start)?;
                let interval = query.pileup.bin_interval(query.interval, bin_index)?;
                let (modification_series, modification_calls) = query
                    .pileup
                    .modifications
                    .bin_index_at(query.interval, record.start)
                    .map_or((0, 0), |index| {
                        query.pileup.modifications.series.iter().fold(
                            (0_usize, 0_u32),
                            |(series_count, calls), series| {
                                let modified =
                                    series.bins.get(index).map_or(0, |bin| bin.modified_calls);
                                (
                                    series_count.saturating_add(usize::from(modified > 0)),
                                    calls.saturating_add(modified),
                                )
                            },
                        )
                    });
                Some(VariantAlignmentEvidence {
                    track_id: track.config.id,
                    track_name: track.config.name.clone(),
                    position: record.start,
                    bin_start: interval.start(),
                    bin_end: interval.end(),
                    depth: bin.depth,
                    forward_depth: bin.forward_depth,
                    reverse_depth: bin.reverse_depth,
                    allele_summary: variant_allele_evidence_summary(record, interval.len(), &bin),
                    modification_series,
                    modification_calls,
                    pair_templates_sampled: query.pairs.sampled_templates,
                    retained_records: query.batch.len(),
                    complete_individual_reads: query.individual_reads_complete(),
                    complete_pileup: query.pileup.provenance.all_eligible_records,
                })
            })
            .collect()
    }

    pub(crate) fn select_variant_alignment_evidence(
        &mut self,
        track_id: TrackId,
        position: u64,
        modification: bool,
    ) {
        self.panels.active_mut().selected_alignment_track = Some(track_id);
        replace_panel_track_selection(
            self.panels.active_mut(),
            TrackFrameKey::Alignment(track_id),
            ui::TrackPane::Annotation,
        );
        self.panels.active_mut().pending_read_selection = None;
        self.panels.active_mut().pending_read_reveal = None;
        self.panels.active_mut().selected_record = None;
        self.panels.active_mut().selected_read_event = None;
        self.panels.active_mut().selected_junction = None;
        self.panels.active_mut().selected_junction_component = None;
        self.panels.active_mut().selected_chain = None;
        self.panels.active_mut().selected_pileup_position = (!modification).then_some(position);
        self.panels.active_mut().selected_modification_position = modification.then_some(position);
        self.rebuild_genomic_display();
        self.display_dirty = true;
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
            &self.panels.active().pane_layout.annotation,
            "Modifications",
            y,
        )?;
        let query = self.alignment_query_for_track(track_id)?;
        modification_position_from_pointer(
            query,
            self.panels.active().viewport,
            panel_rect,
            egui::pos2(x, y),
        )
        .map(|position| (track_id, position))
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
            &self.panels.active().pane_layout.annotation,
            "Coverage",
            y,
        )?;
        let query = self.alignment_query_for_track(track_id)?;
        pileup_position_from_pointer(
            query,
            self.panels.active().viewport,
            panel_rect,
            egui::pos2(x, y),
        )
        .map(|position| (track_id, position))
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
        let panel = self.panels.active();
        let total_targets = panel.junction_arc_targets.len();
        if !panel.navigation_presentation.is_interactive() {
            return JunctionArcHitTestResult {
                gate: SashimiHitTestGate::PresentationUnsettled,
                total_targets,
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
                total_targets,
                ..JunctionArcHitTestResult::default()
            };
        }
        let Some(track_id) =
            alignment_lane_in_pane_layout(&panel.pane_layout.annotation, "Sashimi", y)
        else {
            return JunctionArcHitTestResult {
                gate: SashimiHitTestGate::OutsideSashimiLane,
                total_targets,
                ..JunctionArcHitTestResult::default()
            };
        };
        junction_arc_hit_test_at_point(
            &panel.junction_arc_targets,
            track_id,
            egui::pos2(x, y),
            panel.hovered_junction_arc,
            panel.navigation_presentation.committed_content_revision,
        )
    }

    pub(crate) fn record_at_cursor(&self) -> Option<(TrackId, usize)> {
        if !self
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let scale = self.window.scale_factor();
        let x = (self.cursor.x / scale) as f32;
        let y = (self.cursor.y / scale) as f32;
        let panel_rect = self.active_panel_rect();
        if !panel_rect.contains(egui::pos2(x, y)) {
            return None;
        }
        let (frame, content_y) = self.visible_track_frame_at_screen_y(y, |frame| {
            matches!(frame.key, TrackFrameKey::Alignment(_))
        })?;
        let TrackFrameKey::Alignment(track_id) = frame.key else {
            unreachable!("alignment predicate only returns alignment frames");
        };
        self.complete_read_query_for_track(track_id)?;
        if track_id != self.primary_alignment_track_id() {
            let track = self
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == track_id)?;
            let layout = track
                .projection(self.panels.active().id)?
                .cached_layout
                .as_ref()
                .map(|(_, layout)| layout)?;
            let index =
                alignment_record_at_point(&layout.display_list, x - panel_rect.left(), content_y)?;
            return Some((track_id, index));
        }
        let index = alignment_record_at_point(&self.panels.active().display_list, x, y)?;
        Some((track_id, index))
    }

    pub(crate) fn read_event_at_cursor(
        &self,
        track_id: TrackId,
        record_index: usize,
    ) -> Option<ReadEventSelection> {
        let query = self.complete_read_query_for_track(track_id)?;
        let record = query.materialize_record(record_index).ok()?;
        let scale = self.window.scale_factor();
        let x = self.cursor.x / scale;
        let local_x = x - f64::from(self.active_panel_rect().left());
        let position = self.panels.active().viewport.genomic_at_pixel(local_x);
        let tolerance = (self.panels.active().viewport.bases_per_pixel() * 6.0).max(0.5);
        let include_substitutions = self.panels.active().viewport.bases_per_pixel()
            <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
            && self
                .alignment_filters_for_track(track_id)
                .read_base_display_mode
                != ReadBaseDisplayMode::Hidden;
        let event = find_read_event(
            &record,
            query.reference.as_ref(),
            position,
            tolerance,
            include_substitutions,
        )?;
        let operation = record.cigar.get(event.operation_index)?;
        if matches!(
            operation.kind,
            CigarOpKind::Insertion | CigarOpKind::Deletion
        ) && operation.len < self.primary_alignment_filters().minimum_indel_size
        {
            return None;
        }
        Some(ReadEventSelection {
            record_index,
            operation_index: event.operation_index,
            operation_offset: event.operation_offset,
        })
    }

    pub(crate) fn select_visible_read(&mut self, forward: bool) {
        if !self
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return;
        }
        self.clear_promoted_read();
        self.panels.active_mut().pending_read_selection = None;
        let visible = self
            .panels
            .active()
            .display_list
            .rectangles
            .iter()
            .filter_map(|rectangle| alignment_record_index(rectangle.object_id))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return;
        }
        let selected_position = self
            .panels
            .active()
            .selected_record
            .and_then(|selected| visible.iter().position(|candidate| *candidate == selected));
        let next = if forward {
            selected_position.map_or(0, |position| (position + 1) % visible.len())
        } else {
            selected_position.map_or(visible.len() - 1, |position| {
                position.checked_sub(1).unwrap_or(visible.len() - 1)
            })
        };
        self.panels.active_mut().selected_record = Some(visible[next]);
        self.panels.active_mut().selected_alignment_track = Some(self.primary_alignment_track_id());
        self.panels.active_mut().selected_read_event = None;
        self.panels.active_mut().selected_pileup_position = None;
        self.panels.active_mut().selected_modification_position = None;
        self.panels.active_mut().selected_junction = None;
        self.panels.active_mut().selected_junction_component = None;
        let selected_chain = self
            .complete_read_query_for_track(self.primary_alignment_track_id())
            .and_then(|query| {
                let record = query.materialize_record(visible[next]).ok()?;
                selected_supplementary_chain(&record, &self.panels.active().contig_name)
            });
        self.panels.active_mut().selected_chain = selected_chain;
        self.retain_active_read_selection_for_query_refresh();
        let primary_track_id = self.primary_alignment_track_id();
        replace_panel_track_selection(
            self.panels.active_mut(),
            TrackFrameKey::Alignment(primary_track_id),
            ui::TrackPane::Reads,
        );
        self.display_dirty = true;
    }

    pub(crate) fn clear_semantic_evidence_selection(&mut self) {
        self.panels.active_mut().pending_read_selection = None;
        self.panels.active_mut().pending_read_reveal = None;
        self.panels.active_mut().selected_record = None;
        self.panels.active_mut().selected_alignment_track = None;
        self.panels.active_mut().selected_read_event = None;
        self.panels.active_mut().selected_pileup_position = None;
        self.panels.active_mut().selected_modification_position = None;
        self.panels.active_mut().selected_junction = None;
        self.panels.active_mut().selected_junction_component = None;
        self.panels.active_mut().selected_feature = None;
        self.panels.active_mut().selected_variant = None;
        self.panels.active_mut().selected_gwas = None;
        self.panels.active_mut().selected_quantitative = None;
        self.panels.active_mut().selected_copy_number = None;
        self.panels.active_mut().selected_mutation = None;
        self.panels.active_mut().selected_interaction = None;
        self.panels.active_mut().selected_chain = None;
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn select_loaded_track_evidence(&mut self, forward: bool) {
        if !self
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return;
        }
        let Some(track_key) = self.panels.active().selected_track else {
            "Select a track before moving through its evidence".clone_into(&mut self.data_status);
            return;
        };
        match track_key {
            TrackFrameKey::Group { channel, id } => {
                self.data_status =
                    format!("{channel:?} track group {id} has no discrete evidence records");
            }
            TrackFrameKey::Reference => {
                "The reference track has no discrete evidence records"
                    .clone_into(&mut self.data_status);
            }
            TrackFrameKey::Alignment(track_id) => {
                let Some(query) = self.complete_read_query_for_track(track_id).cloned() else {
                    "The selected alignment track has no complete reads available"
                        .clone_into(&mut self.data_status);
                    return;
                };
                let current = (self.panels.active().selected_alignment_track == Some(track_id))
                    .then_some(self.panels.active().selected_record)
                    .flatten();
                let Some(index) = next_evidence_index(current, query.batch.len(), forward) else {
                    "The selected alignment track has no complete reads available"
                        .clone_into(&mut self.data_status);
                    return;
                };
                let chain = query.materialize_record(index).ok().and_then(|record| {
                    selected_supplementary_chain(&record, &self.panels.active().contig_name)
                });
                self.clear_promoted_read();
                self.clear_semantic_evidence_selection();
                self.panels.active_mut().selected_record = Some(index);
                self.panels.active_mut().selected_alignment_track = Some(track_id);
                self.panels.active_mut().selected_chain = chain;
                replace_panel_track_selection(
                    self.panels.active_mut(),
                    TrackFrameKey::Alignment(track_id),
                    ui::TrackPane::Reads,
                );
                self.data_status = format!(
                    "Selected complete read {} of {} in track {}",
                    index + 1,
                    query.batch.len(),
                    track_id.get()
                );
            }
            TrackFrameKey::Auxiliary(track_id) => {
                let Some(snapshot) = self
                    .panels
                    .active()
                    .track_snapshots
                    .iter()
                    .find(|snapshot| snapshot.id == track_id)
                    .cloned()
                else {
                    "The selected track has no loaded evidence".clone_into(&mut self.data_status);
                    return;
                };
                let selection = match snapshot.data {
                    TrackSnapshotData::Features(records) => {
                        let current = self
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
                    "The selected track has no loaded evidence".clone_into(&mut self.data_status);
                    return;
                };
                self.clear_semantic_evidence_selection();
                let (kind, index) = match selection {
                    EvidenceSelection::Feature(index) => {
                        self.panels.active_mut().selected_feature = Some(FeatureSelection {
                            track_id,
                            record_index: index,
                        });
                        ("feature", index)
                    }
                    EvidenceSelection::Variant(index) => {
                        self.panels.active_mut().selected_variant = Some(VariantSelection {
                            track_id,
                            record_index: index,
                        });
                        ("variant", index)
                    }
                    EvidenceSelection::Gwas(index) => {
                        self.panels.active_mut().selected_gwas = Some(GwasSelection {
                            track_id,
                            record_index: index,
                        });
                        ("GWAS association", index)
                    }
                    EvidenceSelection::Quantitative(index, position) => {
                        self.panels.active_mut().selected_quantitative =
                            Some(QuantitativeSelection {
                                track_id,
                                record_index: index,
                                position,
                            });
                        ("quantitative record", index)
                    }
                    EvidenceSelection::CopyNumber(index) => {
                        self.panels.active_mut().selected_copy_number = Some(CopyNumberSelection {
                            track_id,
                            segment_index: index,
                        });
                        ("copy-number segment", index)
                    }
                    EvidenceSelection::Mutation(index) => {
                        self.panels.active_mut().selected_mutation = Some(MutationSelection {
                            track_id,
                            record_index: index,
                        });
                        ("mutation", index)
                    }
                    EvidenceSelection::Interaction(index) => {
                        self.panels.active_mut().selected_interaction =
                            Some(InteractionSelection {
                                track_id,
                                record_index: index,
                            });
                        ("interaction", index)
                    }
                };
                self.data_status = format!(
                    "Selected loaded {kind} {} in track {}",
                    index + 1,
                    track_id.get()
                );
            }
        }
        self.retain_active_read_selection_for_query_refresh();
        self.display_dirty = true;
    }

    pub(crate) fn promote_selected_read_to_top(&mut self) -> bool {
        let Some(track_id) = self.panels.active().selected_alignment_track else {
            return false;
        };
        let Some(index) = self.panels.active().selected_record else {
            return false;
        };
        let Some(name) = self
            .complete_read_query_for_track(track_id)
            .and_then(|query| query.batch.read_name(index))
            .filter(|name| !name.is_empty())
        else {
            return false;
        };
        let promoted = (track_id, alignment_read_name_hash(name));
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
        let Some((index, chain)) = self
            .complete_read_query_for_track(track_id)
            .and_then(|query| {
                let index = (0..query.batch.len())
                    .find(|index| query.batch.read_name(*index) == Some(name))?;
                let chain = query.materialize_record(index).ok().and_then(|record| {
                    selected_supplementary_chain(&record, &self.panels.active().contig_name)
                });
                Some((index, chain))
            })
        else {
            return false;
        };
        self.clear_promoted_read();
        self.panels.active_mut().pending_read_selection = None;
        self.panels.active_mut().selected_record = Some(index);
        self.panels.active_mut().selected_alignment_track = Some(track_id);
        replace_panel_track_selection(
            self.panels.active_mut(),
            TrackFrameKey::Alignment(track_id),
            ui::TrackPane::Reads,
        );
        self.panels.active_mut().selected_read_event = None;
        self.panels.active_mut().selected_pileup_position = None;
        self.panels.active_mut().selected_modification_position = None;
        self.panels.active_mut().selected_junction = None;
        self.panels.active_mut().selected_junction_component = None;
        self.panels.active_mut().selected_chain = chain;
        self.retain_active_read_selection_for_query_refresh();
        self.display_dirty = true;
        true
    }

    pub(crate) fn clear_promoted_read(&mut self) {
        if self.promoted_read.take().is_some() {
            self.invalidate_alignment_row_layouts();
        }
    }

    pub(crate) fn invalidate_alignment_row_layouts(&mut self) {
        for track in &mut self.alignment_tracks {
            for projection in track.projections.values_mut() {
                projection.alignment_rows = None;
                projection.cached_layout = None;
            }
        }
        self.layout_cache.clear();
        self.display_dirty = true;
    }
}
