use crate::display::auxiliary::{
    measured_feature_label_widths, quantitative_display_records, quantitative_object_id_span,
    quantitative_overlay_members, quantitative_overlay_subtracks, shared_copy_number_range,
    shared_quantitative_range, snapshot_data_record_count, strip_object_ids,
    track_options_with_object_ids,
};
use crate::display::{GenomicDisplayPass, primary_pinned_gene_id};
use crate::ui::gestures::retained_data_overlaps;
use crate::ui::{TrackFrame, TrackFrameKey, TrackSubtrack};
use crate::workspace::context_lanes::{
    ContextLaneDraft, ContextLaneKey, prepared_context_viewport,
};
use crate::{Viewer, ui};
use rgv_core::{
    CopyNumberTrackSettings, MutationTrackSettings, QuantitativeScaleMode, TrackConfig, TrackId,
    TrackKind, Viewport,
};
use rgv_engine::TrackSnapshotData;
use rgv_layout::{
    DisplayList, ObjectIdAllocator, TrackLayoutOptions, layout_copy_number,
    layout_features_with_label_widths, layout_gwas, layout_interactions, layout_mutations,
    layout_quantitative, layout_variants,
};
use std::collections::BTreeSet;

/// Inputs for laying out one visible member of an auxiliary lane.
struct AuxiliaryMemberLane<'a> {
    member: &'a TrackConfig,
    data: &'a TrackSnapshotData,
    source_viewport: Viewport,
    options: TrackLayoutOptions,
    /// Top and bottom of the lane's frame.
    top: f32,
    bottom: f32,
    annotation_clip_bottom: f32,
    effective_auxiliary_configs: &'a [TrackConfig],
    /// Copy-number and mutation settings of the lane's anchor track.
    effective_copy_number_settings: &'a CopyNumberTrackSettings,
    effective_mutation_settings: &'a MutationTrackSettings,
}

impl Viewer {
    /// Every auxiliary track except the pinned gene model, with quantitative
    /// overlay groups drawn once at their first member.
    #[allow(clippy::too_many_lines)]
    pub(super) fn push_auxiliary_lanes(&mut self, pass: &mut GenomicDisplayPass) {
        let annotation_clip_bottom = pass.annotation_clip_bottom;
        let desired_context_interval = pass.desired_context_interval;
        let effective_auxiliary_configs = &pass.effective_auxiliary_configs;
        let pinned_gene_id = primary_pinned_gene_id(effective_auxiliary_configs);
        let viewport_interval = pass.viewport_interval;
        let target_viewport = pass.target_viewport;
        let generation = pass.generation;
        let visible_context_lanes = &mut pass.visible_context_lanes;
        let pending_context_lanes = &mut pass.pending_context_lanes;
        let unavailable_context_lanes = &mut pass.unavailable_context_lanes;
        let context_lane_drafts = &mut pass.context_lane_drafts;
        let object_ids = &mut pass.object_ids;
        let mut top = pass.top;
        let display_configs = effective_auxiliary_configs
            .iter()
            .filter(|track| Some(track.id) != pinned_gene_id)
            .collect::<Vec<_>>();
        let mut rendered_overlay_groups = BTreeSet::new();
        for config in &display_configs {
            let members = if config.kind == TrackKind::Quantitative
                && let Some(group) = config.quantitative.overlay_group
            {
                if !rendered_overlay_groups.insert(group) {
                    continue;
                }
                quantitative_overlay_members(config, &display_configs)
            } else {
                vec![*config]
            };
            let overlay_group = config.quantitative.overlay_group;
            let composite_visible = overlay_group.is_none_or(|group| {
                self.track_presentation
                    .composite_content_visible(ui::TrackPane::Annotation, group)
            });
            let member_content_visible = members.iter().any(|member| member.visible);
            let visible = composite_visible && member_content_visible;
            let collapsed = !visible
                || members
                    .iter()
                    .filter(|member| member.visible)
                    .all(|member| member.collapsed);
            let effective_copy_number_settings = config.copy_number.clone();
            let effective_mutation_settings = config.mutation.clone();
            let height = if collapsed {
                26.0
            } else {
                members
                    .iter()
                    .filter(|member| member.visible && !member.collapsed)
                    .map(|member| member.height.clamp(40.0, 1_200.0))
                    .reduce(f32::max)
                    .unwrap_or(40.0)
            };
            let bottom = top + height;
            let mut subtracks = quantitative_overlay_subtracks(&members, top, height);
            if visible && !collapsed {
                for member in members
                    .iter()
                    .filter(|member| member.visible && !member.collapsed)
                {
                    let key = ContextLaneKey::Auxiliary(member.id);
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
                        .get(&member.id)
                        == Some(&generation)
                    {
                        unavailable_context_lanes.insert(key);
                    }
                }
            }
            if visible && !collapsed {
                for member in members
                    .iter()
                    .filter(|member| member.visible && !member.collapsed)
                {
                    let Some(snapshot) = self
                        .panels
                        .active()
                        .track_snapshots
                        .iter()
                        .find(|snapshot| snapshot.id == member.id)
                    else {
                        continue;
                    };
                    let Some(loaded_interval) = self
                        .panels
                        .active()
                        .track_snapshot_intervals
                        .get(&member.id)
                        .copied()
                        .filter(|loaded| retained_data_overlaps(*loaded, viewport_interval))
                    else {
                        continue;
                    };
                    let source_viewport =
                        prepared_context_viewport(loaded_interval, target_viewport);
                    let options = TrackLayoutOptions {
                        top,
                        height,
                        selected_record: self.selected_auxiliary_record(member.id),
                        feature_display_mode: member.feature.display_mode,
                        ..TrackLayoutOptions::default()
                    };
                    let track_display = self.layout_auxiliary_member(
                        AuxiliaryMemberLane {
                            member,
                            data: &snapshot.data,
                            source_viewport,
                            options,
                            top,
                            bottom,
                            annotation_clip_bottom,
                            effective_auxiliary_configs,
                            effective_copy_number_settings: &effective_copy_number_settings,
                            effective_mutation_settings: &effective_mutation_settings,
                        },
                        &mut subtracks,
                        object_ids,
                    );
                    context_lane_drafts.push(ContextLaneDraft {
                        key: ContextLaneKey::Auxiliary(member.id),
                        generation: self
                            .panels
                            .active()
                            .track_snapshot_generations
                            .get(&member.id)
                            .copied()
                            .expect(
                                "every installed contextual snapshot carries its source generation",
                            ),
                        source_viewport,
                        frame_key: TrackFrameKey::Auxiliary(config.id),
                        frame_top: top,
                        display: track_display,
                    });
                }
            }
            self.panels.active_mut().track_frames.push(TrackFrame {
                key: TrackFrameKey::Auxiliary(config.id),
                channel: ui::TrackPane::Annotation,
                name: overlay_group.map_or_else(
                    || config.name.clone(),
                    |group| format!("Overlay {group} · {} tracks", members.len()),
                ),
                kind: config.kind,
                source_label: Some(
                    members
                        .iter()
                        .map(|member| member.source.redacted_label())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                top,
                bottom,
                visible: overlay_group.map_or(member_content_visible, |_| composite_visible),
                collapsed,
                frozen_bottom: None,
                subtracks,
            });
            top = bottom;
        }
        pass.top = top;
    }

    /// Index of the record selected on `track_id` in the active panel,
    /// whichever kind of auxiliary selection holds it.
    fn selected_auxiliary_record(&self, track_id: TrackId) -> Option<usize> {
        self.panels
            .active()
            .selected_feature
            .filter(|selection| selection.track_id == track_id)
            .map(|selection| selection.record_index)
            .or_else(|| {
                self.panels
                    .active()
                    .selected_variant
                    .filter(|selection| selection.track_id == track_id)
                    .map(|selection| selection.record_index)
            })
            .or_else(|| {
                self.panels
                    .active()
                    .selected_gwas
                    .filter(|selection| selection.track_id == track_id)
                    .map(|selection| selection.record_index)
            })
            .or_else(|| {
                self.panels
                    .active()
                    .selected_quantitative
                    .filter(|selection| selection.track_id == track_id)
                    .map(|selection| selection.record_index)
            })
            .or_else(|| {
                self.panels
                    .active()
                    .selected_interaction
                    .filter(|selection| selection.track_id == track_id)
                    .map(|selection| selection.record_index)
            })
            .or_else(|| {
                self.panels
                    .active()
                    .selected_copy_number
                    .filter(|selection| selection.track_id == track_id)
                    .map(|selection| selection.segment_index)
            })
            .or_else(|| {
                self.panels
                    .active()
                    .selected_mutation
                    .filter(|selection| selection.track_id == track_id)
                    .map(|selection| selection.record_index)
            })
    }

    /// Lays out one visible member of an auxiliary lane from its snapshot
    /// and appends any per-row subtracks the layout exposes.
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::needless_pass_by_value)]
    fn layout_auxiliary_member(
        &self,
        lane: AuxiliaryMemberLane<'_>,
        subtracks: &mut Vec<TrackSubtrack>,
        object_ids: &mut ObjectIdAllocator,
    ) -> DisplayList {
        let AuxiliaryMemberLane {
            member,
            data,
            source_viewport,
            options,
            top,
            bottom,
            annotation_clip_bottom,
            effective_auxiliary_configs,
            effective_copy_number_settings,
            effective_mutation_settings,
        } = lane;
        let quantitative_records = matches!(data, TrackSnapshotData::Quantitative(_))
            .then(|| {
                quantitative_display_records(
                    member,
                    &self.panels.active().track_snapshots,
                    source_viewport,
                )
            })
            .flatten();
        let object_id_span = match data {
            TrackSnapshotData::Quantitative(_) => quantitative_object_id_span(
                quantitative_records.as_deref().map_or(0, <[_]>::len),
                source_viewport,
            ),
            other => snapshot_data_record_count(other),
        };
        let (options, object_id_range) =
            track_options_with_object_ids(object_ids, object_id_span, options);
        let mut display = match data {
            TrackSnapshotData::Features(records) => {
                let label_widths = measured_feature_label_widths(&self.egui_context, records);
                layout_features_with_label_widths(
                    records,
                    source_viewport,
                    options,
                    Some(&label_widths),
                )
                .0
            }
            TrackSnapshotData::Variants(records) => {
                layout_variants(&records.records, source_viewport, options, &member.variant).0
            }
            TrackSnapshotData::Gwas(query) => {
                if query.provenance.records_truncated || query.provenance.source_rows_ignored > 0 {
                    subtracks.push(TrackSubtrack {
                        alignment_subtrack: None,
                        label: if query.provenance.records_truncated {
                            "Sampled points".to_owned()
                        } else {
                            "Missing p-values".to_owned()
                        },
                        detail: Some(if query.provenance.records_truncated {
                            format!(
                                "{} / {}",
                                query.provenance.records_retained,
                                query.provenance.records_examined
                            )
                        } else {
                            format!("{} ignored", query.provenance.source_rows_ignored)
                        }),
                        top: top + 19.0,
                        bottom: (top + 39.0).min(bottom),
                        collapsed: false,
                        composite_member: None,
                    });
                }
                layout_gwas(&query.records, source_viewport, options, &member.gwas).0
            }
            TrackSnapshotData::Quantitative(_) => {
                let shared_range = (member.quantitative.scale_mode
                    == QuantitativeScaleMode::Shared)
                    .then(|| {
                        shared_quantitative_range(
                            effective_auxiliary_configs,
                            &self.panels.active().track_snapshots,
                            source_viewport,
                            member.quantitative.scale_transform,
                            member.quantitative.scale_group,
                        )
                    })
                    .flatten();
                quantitative_records.map_or_else(DisplayList::default, |records| {
                    layout_quantitative(
                        &records,
                        source_viewport,
                        options,
                        &member.quantitative,
                        shared_range,
                    )
                    .0
                })
            }
            TrackSnapshotData::CopyNumber(query) => {
                let shared_range = (member.quantitative.scale_mode
                    == QuantitativeScaleMode::Shared)
                    .then(|| {
                        shared_copy_number_range(
                            member,
                            effective_auxiliary_configs,
                            &self.panels.active().track_snapshots,
                        )
                    })
                    .flatten();
                let layout = layout_copy_number(
                    query,
                    source_viewport,
                    options,
                    effective_copy_number_settings,
                    &member.quantitative,
                    shared_range,
                    self.panels.annotation_scroll_points + self.canvas_geometry().viewport.top(),
                    self.panels.annotation_scroll_points + annotation_clip_bottom,
                );
                if effective_copy_number_settings.show_sample_names {
                    subtracks.extend(layout.visible_rows.iter().filter_map(|row| {
                        query
                            .samples
                            .get(row.sample_index)
                            .map(|sample| TrackSubtrack {
                                alignment_subtrack: None,
                                label: sample.clone(),
                                detail: query
                                    .sample_sort_values
                                    .get(row.sample_index)
                                    .copied()
                                    .flatten()
                                    .map(|value| format!("{value:.3}")),
                                top: row.top,
                                bottom: row.bottom,
                                collapsed: false,
                                composite_member: None,
                            })
                    }));
                }
                layout.display_list
            }
            TrackSnapshotData::Mutations(query) => {
                let layout = layout_mutations(
                    query,
                    source_viewport,
                    options,
                    effective_mutation_settings,
                    self.panels.annotation_scroll_points + self.canvas_geometry().viewport.top(),
                    self.panels.annotation_scroll_points + annotation_clip_bottom,
                );
                if effective_mutation_settings.show_sample_names {
                    subtracks.extend(layout.visible_rows.iter().filter_map(|row| {
                        query
                            .samples
                            .get(row.sample_index)
                            .map(|sample| TrackSubtrack {
                                alignment_subtrack: None,
                                label: sample.clone(),
                                detail: Some("MAF tumour sample".to_owned()),
                                top: row.top,
                                bottom: row.bottom,
                                collapsed: false,
                                composite_member: None,
                            })
                    }));
                }
                layout.display_list
            }
            TrackSnapshotData::Interactions(query) => {
                layout_interactions(
                    &query.records,
                    &self.panels.active().contig_name,
                    source_viewport,
                    options,
                    &member.interaction,
                )
                .0
            }
        };
        if object_id_range.is_none() {
            strip_object_ids(&mut display);
        }
        display
    }
}
