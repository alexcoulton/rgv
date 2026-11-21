use super::*;

impl Viewer {
    /// Reference sequence lane: bases, translation frames, and its frame.
    #[allow(clippy::too_many_lines)]
    pub(super) fn push_reference_lane(&mut self, pass: &mut GenomicDisplayPass) {
        let viewport_interval = pass.viewport_interval;
        let target_viewport = pass.target_viewport;
        let generation = pass.generation;
        let visible_context_lanes = &mut pass.visible_context_lanes;
        let pending_context_lanes = &mut pass.pending_context_lanes;
        let unavailable_context_lanes = &mut pass.unavailable_context_lanes;
        let context_lane_drafts = &mut pass.context_lane_drafts;
        let mut top = pass.top;
        if self.reference_engine.is_some() {
            let frame_top = top;
            let reference_expanded = self.track_presentation.reference_content_visible;
            let sequence_top = frame_top + 25.0;
            let translation_top = sequence_top + 23.0;
            let natural_bottom = if reference_expanded {
                frame_top + 49.0 + self.reference_translation_frames.len() as f32 * 20.0
            } else {
                frame_top + 26.0
            };
            let bottom = if reference_expanded {
                self.track_presentation
                    .reference_preferred_height
                    .map_or(natural_bottom, |height| {
                        frame_top + height.clamp(80.0, 600.0)
                    })
            } else {
                frame_top + 26.0
            };
            let reference_context_interval = buffered_context_interval(
                target_viewport,
                self.panels.active().contig_length,
                self.settings.read_browsing_buffer_percent,
                Some(MAX_REFERENCE_QUERY_BASES),
            );
            if reference_expanded {
                visible_context_lanes.insert(
                    ContextLaneKey::Reference,
                    (TrackFrameKey::Reference, Some(reference_context_interval)),
                );
                if self.panels.active().pending_queries.reference
                    || self.panels.active().reference_settled_generation != Some(generation)
                {
                    pending_context_lanes.insert(ContextLaneKey::Reference);
                }
                if self.panels.active().reference_query_error_generation == Some(generation) {
                    unavailable_context_lanes.insert(ContextLaneKey::Reference);
                }
            }
            if reference_expanded
                && let Some(reference) = self
                    .panels
                    .active()
                    .reference_slice
                    .as_deref()
                    .filter(|_| {
                        self.panels.active().reference_slice_revision == self.reference_revision
                    })
                    .filter(|slice| retained_data_overlaps(slice.interval, viewport_interval))
            {
                let source_viewport =
                    prepared_context_viewport(reference.interval, target_viewport);
                let reference_display = layout_reference_slice_oriented(
                    reference,
                    source_viewport,
                    sequence_top,
                    self.reference_reverse_complement,
                );
                let mut lane_display = reference_display;
                for (index, frame) in self
                    .reference_translation_frames
                    .iter()
                    .copied()
                    .enumerate()
                {
                    let translation = layout_reference_translation(
                        reference,
                        source_viewport,
                        translation_top + index as f32 * 20.0,
                        frame,
                        self.reference_genetic_code,
                    );
                    lane_display.rectangles.extend(translation.rectangles);
                    lane_display.texts.extend(translation.texts);
                }
                context_lane_drafts.push(ContextLaneDraft {
                    key: ContextLaneKey::Reference,
                    generation: self
                        .panels
                        .active()
                        .reference_slice_generation
                        .expect("every installed reference slice carries its source generation"),
                    source_viewport,
                    frame_key: TrackFrameKey::Reference,
                    frame_top,
                    display: lane_display,
                });
            }
            let mut subtracks = Vec::new();
            if reference_expanded {
                let bases_per_pixel = self.panels.active().viewport.bases_per_pixel();
                let reference_detail = if bases_per_pixel > MAXIMUM_BASE_MARK_BASES_PER_PIXEL {
                    Some("zoom in to see sequence".to_owned())
                } else if 1.0 / bases_per_pixel < f64::from(MINIMUM_BASE_GLYPH_PIXEL_WIDTH) {
                    Some("zoom in to see base letters".to_owned())
                } else {
                    None
                };
                subtracks.push(TrackSubtrack {
                    alignment_subtrack: None,
                    label: reference_bases_label(self.reference_reverse_complement),
                    detail: reference_detail,
                    top: sequence_top,
                    bottom: sequence_top + 20.0,
                    collapsed: false,
                    composite_member: None,
                });
                subtracks.extend(
                    self.reference_translation_frames
                        .iter()
                        .copied()
                        .enumerate()
                        .map(|(index, frame)| TrackSubtrack {
                            alignment_subtrack: None,
                            label: format!("Translation {}", frame.label()),
                            detail: Some(format!("{:?}", self.reference_genetic_code)),
                            top: translation_top + index as f32 * 20.0,
                            bottom: translation_top + (index + 1) as f32 * 20.0,
                            collapsed: false,
                            composite_member: None,
                        }),
                );
            }
            let frame = TrackFrame {
                key: TrackFrameKey::Reference,
                channel: ui::TrackPane::Annotation,
                name: "Reference sequence".to_owned(),
                kind: TrackKind::Reference,
                source_label: self
                    .reference_source
                    .as_ref()
                    .map(TrackSource::redacted_label),
                top: frame_top,
                bottom,
                visible: true,
                collapsed: !reference_expanded,
                frozen_bottom: None,
                subtracks,
            };
            self.panels.active_mut().track_frames.push(frame);
            top = bottom;
        }
        pass.top = top;
    }

    /// The primary gene-model track, pinned directly below the reference.
    #[allow(clippy::too_many_lines)]
    pub(super) fn push_pinned_gene_lanes(&mut self, pass: &mut GenomicDisplayPass) {
        let desired_context_interval = pass.desired_context_interval;
        let effective_auxiliary_configs = &pass.effective_auxiliary_configs;
        let viewport_interval = pass.viewport_interval;
        let target_viewport = pass.target_viewport;
        let generation = pass.generation;
        let visible_context_lanes = &mut pass.visible_context_lanes;
        let pending_context_lanes = &mut pass.pending_context_lanes;
        let unavailable_context_lanes = &mut pass.unavailable_context_lanes;
        let context_lane_drafts = &mut pass.context_lane_drafts;
        let mut top = pass.top;
        // Gene models are reference context, so keep them immediately above the
        // primary alignment instead of placing them below an arbitrarily tall
        // read stack. This preserves visible gene context while reads scroll.
        // Keep one primary annotation alongside the reference as persistent
        // genomic context. Additional gene-model tracks are comparative data:
        // pinning every one can consume the complete viewport and leave no
        // visible scrollable region (MF-10 has seven of them).
        let pinned_gene_id = primary_pinned_gene_id(effective_auxiliary_configs);
        let pinned_gene_configs = effective_auxiliary_configs
            .iter()
            .filter(|track| Some(track.id) == pinned_gene_id)
            .cloned()
            .collect::<Vec<_>>();
        for config in pinned_gene_configs {
            let frame_top = top;
            // A density gesture may resize the scrollable stack, but persistent
            // context must not move the scroll clip boundary underneath it.
            let height = pinned_gene_track_height(&config);
            let bottom = frame_top + height;
            if !config.collapsed {
                let key = ContextLaneKey::Auxiliary(config.id);
                visible_context_lanes.insert(
                    key,
                    (
                        TrackFrameKey::Auxiliary(config.id),
                        Some(desired_context_interval),
                    ),
                );
                if self.panels.active().pending_queries.tracks
                    || self.panels.active().track_settled_generation != Some(generation)
                {
                    pending_context_lanes.insert(key);
                }
                if self
                    .panels
                    .active()
                    .track_query_error_generations
                    .get(&config.id)
                    == Some(&generation)
                {
                    unavailable_context_lanes.insert(key);
                }
            }
            if !config.collapsed
                && let Some(loaded_interval) = self
                    .panels
                    .active()
                    .track_snapshot_intervals
                    .get(&config.id)
                    .copied()
                    .filter(|loaded| retained_data_overlaps(*loaded, viewport_interval))
                && let Some(TrackSnapshot {
                    data: TrackSnapshotData::Features(records),
                    ..
                }) = self
                    .panels
                    .active()
                    .track_snapshots
                    .iter()
                    .find(|snapshot| snapshot.id == config.id)
            {
                let source_viewport = prepared_context_viewport(loaded_interval, target_viewport);
                let options = TrackLayoutOptions {
                    top: frame_top,
                    height,
                    maximum_rows: pinned_gene_layout_maximum_rows(height),
                    selected_record: self
                        .panels
                        .active()
                        .selected_feature
                        .filter(|selection| selection.track_id == config.id)
                        .map(|selection| selection.record_index),
                    object_id_base: 1_000_000_u32.saturating_add(
                        u32::try_from(config.id.get())
                            .unwrap_or(u32::MAX)
                            .saturating_mul(100_000),
                    ),
                    feature_display_mode: config.feature.display_mode,
                    ..TrackLayoutOptions::default()
                };
                let label_widths = measured_feature_label_widths(&self.egui_context, records);
                let gene_display = layout_features_with_label_widths(
                    records,
                    source_viewport,
                    options,
                    Some(&label_widths),
                )
                .0;
                context_lane_drafts.push(ContextLaneDraft {
                    key: ContextLaneKey::Auxiliary(config.id),
                    generation: self
                        .panels
                        .active()
                        .track_snapshot_generations
                        .get(&config.id)
                        .copied()
                        .expect(
                            "every installed contextual snapshot carries its source generation",
                        ),
                    source_viewport,
                    frame_key: TrackFrameKey::Auxiliary(config.id),
                    frame_top,
                    display: gene_display,
                });
            }
            self.panels.active_mut().track_frames.push(TrackFrame {
                key: TrackFrameKey::Auxiliary(config.id),
                channel: ui::TrackPane::Annotation,
                name: config.name,
                kind: config.kind,
                source_label: Some(config.source.redacted_label()),
                top: frame_top,
                bottom,
                visible: true,
                collapsed: config.collapsed,
                frozen_bottom: None,
                subtracks: Vec::new(),
            });
            top = bottom;
        }
        pass.top = top;
    }
}
