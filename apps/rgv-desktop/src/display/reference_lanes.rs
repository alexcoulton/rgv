use crate::display::auxiliary::{
    feature_presentation, measured_feature_label_widths, pinned_gene_layout_maximum_rows,
    strip_object_ids, track_options_with_object_ids,
};
use crate::display::{
    GenomicDisplayPass, pinned_gene_track_height, primary_pinned_gene_id, reference_bases_label,
};
use crate::ui::gestures::retained_data_overlaps;
use crate::ui::{TrackFrame, TrackFrameKey, TrackSubtrack};
use crate::workspace::context_lanes::{
    ContextLaneDraft, ContextLaneKey, buffered_context_interval, prepared_context_viewport,
};
use crate::{Viewer, ui};
use rgv_core::PanelId;
use rgv_core::{
    GeneticCode, MAXIMUM_BASE_MARK_BASES_PER_PIXEL, ReferenceTranslationFrame, TrackKind,
    TrackSource, Viewport,
};
use rgv_engine::{MAX_REFERENCE_QUERY_BASES, TrackSnapshot, TrackSnapshotData};
use rgv_hts::ReferenceSlice;
use rgv_layout::{
    DisplayList, MINIMUM_BASE_GLYPH_PIXEL_WIDTH, TrackLayoutOptions, layout_presented_features,
    layout_reference_slice_oriented, layout_reference_translation_in_contig,
};

/// Appends one 20-point lane per translation frame. Reverse frames are
/// phased from the contig end, so the contig length is required.
pub(crate) fn append_reference_translation_lanes(
    display: &mut DisplayList,
    reference: &ReferenceSlice,
    viewport: Viewport,
    translation_top: f32,
    frames: &[ReferenceTranslationFrame],
    genetic_code: GeneticCode,
    contig_length: u64,
) {
    for (index, frame) in frames.iter().copied().enumerate() {
        let translation = layout_reference_translation_in_contig(
            reference,
            viewport,
            translation_top + index as f32 * 20.0,
            frame,
            genetic_code,
            Some(contig_length),
        );
        display.rectangles.extend(translation.rectangles);
        display.texts.extend(translation.texts);
    }
}

impl Viewer {
    /// Reference sequence lane: bases, translation frames, and its frame.
    #[allow(clippy::too_many_lines)]
    pub(super) fn push_reference_lane(&mut self, panel_id: PanelId, pass: &mut GenomicDisplayPass) {
        let viewport_interval = pass.viewport_interval;
        let target_viewport = pass.target_viewport;
        let generation = pass.generation;
        let visible_context_lanes = &mut pass.visible_context_lanes;
        let pending_context_lanes = &mut pass.pending_context_lanes;
        let unavailable_context_lanes = &mut pass.unavailable_context_lanes;
        let context_lane_drafts = &mut pass.context_lane_drafts;
        let mut top = pass.top;
        if self.session.reference_engine.is_some() {
            let frame_top = top;
            let reference_expanded = self.session.track_presentation.reference_content_visible;
            let sequence_top = frame_top + 25.0;
            let translation_top = sequence_top + 23.0;
            let natural_bottom = if reference_expanded {
                frame_top + 49.0 + self.reference_translation_frames.len() as f32 * 20.0
            } else {
                frame_top + 26.0
            };
            let bottom = if reference_expanded {
                self.session
                    .track_presentation
                    .reference_preferred_height
                    .map_or(natural_bottom, |height| {
                        frame_top + height.clamp(80.0, 600.0)
                    })
            } else {
                frame_top + 26.0
            };
            let reference_context_interval = buffered_context_interval(
                target_viewport,
                self.session.panels.panel(panel_id).contig_length,
                self.session.settings.read_browsing_buffer_percent,
                Some(MAX_REFERENCE_QUERY_BASES),
            );
            let sequence_drawn = reference_sequence_is_drawn(
                self.session.panels.panel(panel_id).viewport.bases_per_pixel(),
            );
            if reference_expanded {
                visible_context_lanes.insert(
                    ContextLaneKey::Reference,
                    (TrackFrameKey::Reference, Some(reference_context_interval)),
                );
                // Zoomed out, no reference is fetched for display, so nothing is pending.
                if sequence_drawn
                    && (self
                        .session
                        .panels
                        .panel(panel_id)
                        .pending_queries
                        .reference
                        || self
                            .session
                            .panels
                            .panel(panel_id)
                            .reference_settled_generation
                            != Some(generation))
                {
                    pending_context_lanes.insert(ContextLaneKey::Reference);
                }
                if self
                    .session
                    .panels
                    .panel(panel_id)
                    .reference_query_error_generation
                    == Some(generation)
                {
                    unavailable_context_lanes.insert(ContextLaneKey::Reference);
                }
            }
            if reference_expanded && !sequence_drawn {
                // Publish an empty lane rather than nothing: a lane with no draft keeps its
                // previous lease on screen while it overlaps the view, so a sequence drawn at a
                // closer zoom would otherwise stay up, rescaled, as a partial colour band.
                context_lane_drafts.push(ContextLaneDraft {
                    key: ContextLaneKey::Reference,
                    generation,
                    source_viewport: target_viewport,
                    frame_key: TrackFrameKey::Reference,
                    frame_top,
                    display: DisplayList::default(),
                });
            } else if reference_expanded
                && let Some(reference) = self
                    .session
                    .panels
                    .panel(panel_id)
                    .reference_slice
                    .as_deref()
                    .filter(|_| {
                        self.session.panels.panel(panel_id).reference_slice_revision
                            == self.session.reference_revision
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
                append_reference_translation_lanes(
                    &mut lane_display,
                    reference,
                    source_viewport,
                    translation_top,
                    &self.reference_translation_frames,
                    self.reference_genetic_code,
                    self.session.panels.panel(panel_id).contig_length,
                );
                context_lane_drafts.push(ContextLaneDraft {
                    key: ContextLaneKey::Reference,
                    generation: self
                        .session
                        .panels
                        .panel(panel_id)
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
                let bases_per_pixel = self
                    .session
                    .panels
                    .panel(panel_id)
                    .viewport
                    .bases_per_pixel();
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
                    .session
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
            self.session
                .panels
                .panel_mut(panel_id)
                .track_frames
                .push(frame);
            top = bottom;
        }
        pass.top = top;
    }

    /// The primary gene-model track, pinned directly below the reference.
    #[allow(clippy::too_many_lines)]
    pub(super) fn push_pinned_gene_lanes(
        &mut self,
        panel_id: PanelId,
        pass: &mut GenomicDisplayPass,
    ) {
        let desired_context_interval = pass.desired_context_interval;
        let effective_auxiliary_configs = &pass.effective_auxiliary_configs;
        let viewport_interval = pass.viewport_interval;
        let target_viewport = pass.target_viewport;
        let generation = pass.generation;
        let visible_context_lanes = &mut pass.visible_context_lanes;
        let pending_context_lanes = &mut pass.pending_context_lanes;
        let unavailable_context_lanes = &mut pass.unavailable_context_lanes;
        let context_lane_drafts = &mut pass.context_lane_drafts;
        let object_ids = &mut pass.object_ids;
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
                if self.session.panels.panel(panel_id).pending_queries.tracks
                    || self.session.panels.panel(panel_id).track_settled_generation
                        != Some(generation)
                {
                    pending_context_lanes.insert(key);
                }
                if self
                    .session
                    .panels
                    .panel(panel_id)
                    .track_query_error_generations
                    .get(&config.id)
                    == Some(&generation)
                {
                    unavailable_context_lanes.insert(key);
                }
            }
            if !config.collapsed
                && let Some(loaded_interval) = self
                    .session
                    .panels
                    .panel(panel_id)
                    .track_snapshot_intervals
                    .get(&config.id)
                    .copied()
                    .filter(|loaded| retained_data_overlaps(*loaded, viewport_interval))
                && let Some(TrackSnapshot {
                    data: TrackSnapshotData::Features(records),
                    ..
                }) = self
                    .session
                    .panels
                    .panel(panel_id)
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
                        .session
                        .panels
                        .panel(panel_id)
                        .selected_feature
                        .filter(|selection| selection.track_id == config.id)
                        .map(|selection| selection.record_index),
                    feature_display_mode: config.feature.display_mode,
                    ..TrackLayoutOptions::default()
                };
                let (options, object_id_range) =
                    track_options_with_object_ids(object_ids, records.len(), options);
                let presentation = feature_presentation(&config, records);
                let label_widths = measured_feature_label_widths(
                    &self.egui_context,
                    records,
                    presentation.as_ref(),
                );
                let mut gene_display = layout_presented_features(
                    records,
                    source_viewport,
                    options,
                    Some(&label_widths),
                    presentation.as_ref(),
                )
                .0;
                if object_id_range.is_none() {
                    strip_object_ids(&mut gene_display);
                }
                context_lane_drafts.push(ContextLaneDraft {
                    key: ContextLaneKey::Auxiliary(config.id),
                    generation: self
                        .session
                        .panels
                        .panel(panel_id)
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
            self.session
                .panels
                .panel_mut(panel_id)
                .track_frames
                .push(TrackFrame {
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

/// Whether the reference lane draws sequence at `bases_per_pixel`: the same zoom up to which the
/// session fetches reference for display (`MAXIMUM_BASE_MARK_BASES_PER_PIXEL`). Further out the
/// lane reads "zoom in to see sequence", and a slice still held from a closer view (or fetched for
/// the alignment scene's mismatch evidence) must not be drawn as a partial colour band.
fn reference_sequence_is_drawn(bases_per_pixel: f64) -> bool {
    bases_per_pixel <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
}

#[cfg(test)]
mod reference_zoom_tests {
    use super::reference_sequence_is_drawn;
    use rgv_core::MAXIMUM_BASE_MARK_BASES_PER_PIXEL;

    #[test]
    fn retained_reference_is_drawn_only_while_sequence_is_fetched_for_display() {
        assert!(reference_sequence_is_drawn(0.01));
        assert!(reference_sequence_is_drawn(MAXIMUM_BASE_MARK_BASES_PER_PIXEL));
        // The screenshot's 133 kb view: a retained 11 kb slice must not be drawn there.
        assert!(!reference_sequence_is_drawn(96.159));
        assert!(!reference_sequence_is_drawn(
            MAXIMUM_BASE_MARK_BASES_PER_PIXEL * 1.0001
        ));
    }
}
