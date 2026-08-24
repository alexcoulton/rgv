pub(crate) mod alignment;
pub(crate) mod auxiliary;
pub(crate) mod hit_test;

use super::*;

impl Viewer {
    pub(crate) fn ensure_alignment_rows(&mut self) {
        let panel_id = self.panels.active().id;
        let primary_id = self.primary_alignment_track_id();
        let filters = self.primary_alignment_filters().clone();
        let Some(query) = self.alignment_query_for_track(primary_id).cloned() else {
            return;
        };
        let max_rows = self.primary_alignment_limit().clamp(90, 4_096);
        let pair_length_bounds = self.effective_template_length_bounds(&query);
        let sort_position =
            alignment_sort_position(filters.alignment_sort_mode, self.panels.active().viewport);
        let promoted_read_name_hash = self
            .promoted_read
            .filter(|(track_id, _)| *track_id == primary_id)
            .map(|(_, hash)| hash);
        let rows_match = self
            .alignment_tracks
            .first()
            .and_then(|track| track.projection(panel_id))
            .and_then(|projection| projection.alignment_rows.as_ref())
            .is_some_and(|cached| {
                Arc::ptr_eq(&cached.query, &query)
                    && cached.group_mode == filters.alignment_group_mode
                    && cached.group_order == filters.alignment_group_order
                    && cached.sort_mode == filters.alignment_sort_mode
                    && cached.sort_order == filters.alignment_sort_order
                    && cached.sort_position == sort_position
                    && cached.promoted_read_name_hash == promoted_read_name_hash
                    && cached.max_rows == max_rows
                    && cached.view_as_pairs == filters.view_as_pairs
                    && cached.pair_length_bounds == pair_length_bounds
            });
        if rows_match {
            return;
        }
        let options = AlignmentLayoutOptions {
            group_mode: filters.alignment_group_mode,
            group_order: filters.alignment_group_order,
            sort_mode: filters.alignment_sort_mode,
            sort_order: filters.alignment_sort_order,
            sort_position,
            promoted_read_name_hash,
            max_rows,
            view_as_pairs: filters.view_as_pairs,
            pair_length_bounds,
            ..AlignmentLayoutOptions::default()
        };
        let previous = self
            .alignment_tracks
            .first()
            .and_then(|track| track.projection(panel_id))
            .and_then(|projection| projection.alignment_rows.as_ref())
            .and_then(|cached| {
                (cached.group_mode == filters.alignment_group_mode
                    && cached.group_order == filters.alignment_group_order
                    && cached.sort_mode == filters.alignment_sort_mode
                    && cached.sort_order == filters.alignment_sort_order
                    && cached.sort_position == sort_position
                    && cached.promoted_read_name_hash == promoted_read_name_hash
                    && cached.max_rows == max_rows
                    && cached.view_as_pairs == filters.view_as_pairs
                    && cached.pair_length_bounds == pair_length_bounds)
                    .then_some(cached.layout.as_ref())
            });
        let layout = Arc::new(assign_alignment_rows(&query, options, previous));
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.projection_mut(panel_id).alignment_rows = Some(CachedAlignmentRows {
                query,
                layout,
                group_mode: filters.alignment_group_mode,
                group_order: filters.alignment_group_order,
                sort_mode: filters.alignment_sort_mode,
                sort_order: filters.alignment_sort_order,
                sort_position,
                promoted_read_name_hash,
                max_rows,
                view_as_pairs: filters.view_as_pairs,
                pair_length_bounds,
            });
        }
        self.layout_cache.clear();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn rebuild_genomic_display(&mut self) {
        self.ensure_alignment_rows();
        let shared_sashimi_raw = self.shared_sashimi_maximum(SashimiCountMode::Raw);
        let shared_sashimi_cpm = self.shared_sashimi_maximum(SashimiCountMode::CountsPerMillion);
        let primary_alignment_id = self.primary_alignment_track_id();
        let primary_alignment_name = self.alignment_track_name();
        let primary_source_label = self
            .primary_alignment_source()
            .map(TrackSource::redacted_label);
        let mut display = DisplayList::default();
        let mut pinned_display = DisplayList::default();
        let mut top = self.canvas_geometry.viewport.top();
        let mut scroll_clip_top = top;
        self.panels.active_mut().track_frames.clear();
        let viewport_interval = self.panels.active().viewport.interval();
        let junction_annotations = junction_annotation_context(
            &self.tracks,
            &self.panels.active().track_snapshots,
            self.panels.active().track_snapshot_interval,
            viewport_interval,
        );
        let tracks_overlap_viewport = self
            .panels
            .active()
            .track_snapshot_interval
            .is_some_and(|loaded| retained_data_overlaps(loaded, viewport_interval));
        let supports_fast_scroll = !self.tracks.tracks().iter().any(|track| {
            track.visible
                && !track.collapsed
                && matches!(track.kind, TrackKind::CopyNumber | TrackKind::Mutation)
        });
        if self.primary_alignment_filters().show_reference && self.reference_engine.is_some() {
            let frame_top = top;
            let sequence_top = frame_top + 25.0;
            let translation_top = sequence_top + 23.0;
            let bottom = frame_top + 49.0 + self.reference_translation_frames.len() as f32 * 20.0;
            if let Some(reference) = self
                .panels
                .active()
                .reference_slice
                .as_deref()
                .filter(|slice| retained_data_overlaps(slice.interval, viewport_interval))
            {
                let reference_display = layout_reference_slice_oriented(
                    reference,
                    self.panels.active().viewport,
                    sequence_top,
                    self.reference_reverse_complement,
                );
                pinned_display
                    .rectangles
                    .extend(reference_display.rectangles);
                pinned_display.texts.extend(reference_display.texts);
                for (index, frame) in self
                    .reference_translation_frames
                    .iter()
                    .copied()
                    .enumerate()
                {
                    let translation = layout_reference_translation(
                        reference,
                        self.panels.active().viewport,
                        translation_top + index as f32 * 20.0,
                        frame,
                        self.reference_genetic_code,
                    );
                    pinned_display.rectangles.extend(translation.rectangles);
                    pinned_display.texts.extend(translation.texts);
                }
            }
            let reference_is_zoomed_out =
                self.panels.active().viewport.detail_level() != DetailLevel::Bases;
            let mut subtracks = vec![TrackSubtrack {
                label: if self.reference_reverse_complement {
                    "Bases · reverse strand".to_owned()
                } else {
                    "Bases · forward".to_owned()
                },
                detail: reference_is_zoomed_out.then(|| "zoom in to see sequence".to_owned()),
                top: sequence_top,
                bottom: sequence_top + 20.0,
            }];
            subtracks.extend(
                self.reference_translation_frames
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, frame)| TrackSubtrack {
                        label: format!("Translation {}", frame.label()),
                        detail: Some(format!("{:?}", self.reference_genetic_code)),
                        top: translation_top + index as f32 * 20.0,
                        bottom: translation_top + (index + 1) as f32 * 20.0,
                    }),
            );
            let frame = TrackFrame {
                key: TrackFrameKey::Reference,
                name: "Reference sequence".to_owned(),
                kind: TrackKind::Reference,
                source_label: self
                    .reference_source
                    .as_ref()
                    .map(TrackSource::redacted_label),
                top: frame_top,
                bottom,
                visible: true,
                collapsed: false,
                frozen_bottom: Some(bottom),
                subtracks,
            };
            self.panels.active_mut().track_frames.push(frame);
            top = bottom;
            scroll_clip_top = bottom;
        }
        if self.primary_alignment_visible()
            && !self.primary_alignment_collapsed()
            && let Some(query) = self
                .alignment_query_for_track(primary_alignment_id)
                .cloned()
                .filter(|query| retained_data_overlaps(query.interval, viewport_interval))
        {
            let mut options = AlignmentLayoutOptions::default();
            let density = self.primary_alignment_filters().alignment_density;
            let secondary_scale = density.sqrt();
            let show_modification_frequency =
                self.primary_alignment_filters().show_modification_frequency
                    && self.panels.active().viewport.detail_level() != DetailLevel::Overview;
            let show_junctions = self.primary_alignment_filters().show_junctions
                && !query.junctions.junctions.is_empty();
            let embedded_reference =
                self.primary_alignment_filters().show_reference && self.reference_engine.is_none();
            let reference_height = (28.0 * secondary_scale).clamp(20.0, 52.0);
            let frame_top = top;
            let content_top = frame_top + 22.0;
            let mut lane_top = content_top;
            options.show_reference = embedded_reference;
            options.reference_top = lane_top;
            if embedded_reference {
                // Reserve a stable reference band at every horizontal scale. Its
                // contents appear at base detail, but later lanes must not move
                // when a horizontal gesture crosses an LOD boundary.
                lane_top += reference_height;
            }
            options.show_coverage = self.primary_alignment_filters().show_coverage;
            options.coverage_top = lane_top;
            options.coverage_height = (48.0 * secondary_scale).clamp(28.0, 84.0);
            options.coverage_scale_maximum =
                self.primary_alignment_filters().coverage_scale_maximum;
            if self.primary_alignment_filters().show_coverage {
                lane_top += options.coverage_height + 6.0;
            }
            options.show_junctions = show_junctions;
            options.junction_top = lane_top;
            options.junction_height = match self.primary_alignment_filters().junction_display_mode {
                JunctionDisplayMode::Collapsed => (34.0 * secondary_scale).clamp(24.0, 58.0),
                JunctionDisplayMode::Expanded => (104.0 * secondary_scale).clamp(72.0, 180.0),
            };
            options.minimum_junction_depth =
                self.primary_alignment_filters().minimum_junction_depth;
            options.junction_display_mode = self.primary_alignment_filters().junction_display_mode;
            options.junction_count_mode = self.primary_alignment_filters().sashimi_count_mode;
            options.show_junction_labels = self.primary_alignment_filters().show_junction_labels;
            options.junction_annotation_fingerprint = junction_annotations.lookup.fingerprint();
            options.junction_scale_maximum =
                match self.primary_alignment_filters().sashimi_scale_mode {
                    SashimiScaleMode::Automatic => None,
                    SashimiScaleMode::Fixed => Some(f64::from(
                        self.primary_alignment_filters().sashimi_fixed_scale_maximum,
                    )),
                    SashimiScaleMode::Shared => {
                        match self.primary_alignment_filters().sashimi_count_mode {
                            SashimiCountMode::Raw => shared_sashimi_raw,
                            SashimiCountMode::CountsPerMillion => shared_sashimi_cpm,
                        }
                    }
                };
            options.selected_junction = if self.panels.active_mut().selected_alignment_track
                == Some(primary_alignment_id)
            {
                self.panels
                    .active()
                    .selected_junction
                    .map_or([u64::MAX; 2], |(start, end)| [start, end])
            } else {
                [u64::MAX; 2]
            };
            if show_junctions {
                lane_top += options.junction_height + 6.0;
            }
            options.show_modification_frequency = show_modification_frequency;
            options.modification_top = lane_top;
            options.modification_height = (26.0 * secondary_scale).clamp(20.0, 40.0);
            if show_modification_frequency {
                lane_top += options.modification_height + 8.0;
            }
            options.show_reads = self.primary_alignment_filters().show_reads;
            options.reads_top = lane_top
                + if self.primary_alignment_filters().show_reads {
                    1.0
                } else {
                    0.0
                };
            options.row_height = (7.0 * density).clamp(2.5, MAX_ALIGNMENT_ROW_HEIGHT);
            options.read_height = (5.0 * density).clamp(1.5, (options.row_height - 1.0).max(1.5));
            options.color_mode = self.primary_alignment_filters().alignment_color_mode;
            options.group_mode = self.primary_alignment_filters().alignment_group_mode;
            options.group_order = self.primary_alignment_filters().alignment_group_order;
            options.sort_mode = self.primary_alignment_filters().alignment_sort_mode;
            options.sort_order = self.primary_alignment_filters().alignment_sort_order;
            options.sort_position = alignment_sort_position(
                self.primary_alignment_filters().alignment_sort_mode,
                self.panels.active().viewport,
            );
            options.view_as_pairs = self.primary_alignment_filters().view_as_pairs;
            options.pair_length_bounds = self.effective_template_length_bounds(&query);
            options.minimum_allele_fraction =
                self.primary_alignment_filters().minimum_allele_fraction;
            options.quality_weighted_alleles =
                self.primary_alignment_filters().quality_weighted_alleles;
            options.show_coverage_center_line =
                self.primary_alignment_filters().show_coverage_center_line;
            options.selected_pileup_position = (self.panels.active_mut().selected_alignment_track
                == Some(primary_alignment_id))
            .then_some(self.panels.active().selected_pileup_position)
            .flatten();
            options.read_base_display_mode =
                self.primary_alignment_filters().read_base_display_mode;
            options.shade_read_bases_by_quality =
                self.primary_alignment_filters().shade_read_bases_by_quality;
            options.minimum_indel_size = self.primary_alignment_filters().minimum_indel_size;
            options.minimum_event_label_size =
                self.primary_alignment_filters().minimum_event_label_size;
            options.show_modified_bases = self.primary_alignment_filters().show_modified_bases;
            options.selected_modification_position =
                (self.panels.active_mut().selected_alignment_track == Some(primary_alignment_id))
                    .then_some(self.panels.active().selected_modification_position)
                    .flatten();
            options.selected_chain_read_name_hash =
                (self.panels.active_mut().selected_alignment_track == Some(primary_alignment_id))
                    .then(|| {
                        self.panels
                            .active()
                            .selected_chain
                            .as_ref()
                            .map(|chain| alignment_read_name_hash(&chain.read_name))
                    })
                    .flatten();
            options.promoted_read_name_hash = self
                .promoted_read
                .filter(|(track_id, _)| *track_id == primary_alignment_id)
                .map(|(_, hash)| hash);
            options.selected_chain_read_name_hash = options
                .selected_chain_read_name_hash
                .or(options.promoted_read_name_hash);
            options.minimum_modification_probability = self
                .primary_alignment_filters()
                .minimum_modification_probability;
            options.modified_base_color_mode =
                self.primary_alignment_filters().modified_base_color_mode;
            for (slot, code) in options
                .hidden_modification_codes
                .iter_mut()
                .zip(self.primary_hidden_modification_codes())
            {
                *slot = Some(code);
            }
            options.max_rows = (self.primary_alignment_limit()).clamp(90, 4_096);
            let key = alignment_layout_key(
                PanelTrackId::new(self.panels.active().id, primary_alignment_id),
                self.primary_alignment_source(),
                self.panels.active().viewport,
                query.interval,
                self.primary_active_filters(),
                &options,
                top,
            );
            let layout = if let Some(layout) = self.layout_cache.get(&key).cloned() {
                layout
            } else {
                let rows = self
                    .alignment_tracks
                    .first()
                    .and_then(|track| track.projection(self.panels.active().id))
                    .and_then(|projection| projection.alignment_rows.as_ref())
                    .expect("alignment rows exist whenever an alignment query exists");
                let layout = Arc::new(layout_alignment_query_with_rows_and_annotations(
                    &query,
                    self.panels.active().viewport,
                    options,
                    &rows.layout,
                    Some(&junction_annotations.lookup),
                ));
                let weight = layout
                    .display_list
                    .rectangles
                    .len()
                    .saturating_mul(std::mem::size_of::<rgv_layout::RectPrimitive>());
                let _ = self.layout_cache.insert(key, Arc::clone(&layout), weight);
                layout
            };
            extend_alignment_layers(&mut pinned_display, &mut display, &layout);
            scroll_clip_top = options.reads_top;
            if let Some(track) = self.alignment_tracks.first_mut() {
                track.projection_mut(self.panels.active().id).cached_layout =
                    Some((key, Arc::clone(&layout)));
            }
            let pinned_bottom = if show_modification_frequency {
                options.modification_top + options.modification_height
            } else if show_junctions {
                options.junction_top + options.junction_height
            } else if self.primary_alignment_filters().show_coverage {
                options.coverage_top + options.coverage_height
            } else if embedded_reference {
                options.reference_top + reference_height
            } else {
                content_top
            };
            let natural_bottom = if self.primary_alignment_filters().show_reads {
                layout
                    .display_list
                    .rectangles
                    .iter()
                    .skip(layout.pinned_rectangle_count)
                    .map(|rectangle| rectangle.y + rectangle.height)
                    .fold(options.reads_top + options.row_height, f32::max)
                    + 6.0
            } else {
                pinned_bottom.max(frame_top + 26.0) + 6.0
            };
            let bottom = alignment_frame_bottom(
                frame_top,
                natural_bottom,
                self.shared_primary_alignment_height,
            );
            let alignment_frame = TrackFrame {
                key: TrackFrameKey::Alignment(primary_alignment_id),
                name: primary_alignment_name.clone(),
                kind: TrackKind::Alignment,
                source_label: primary_source_label.clone(),
                top: frame_top,
                bottom,
                visible: true,
                collapsed: false,
                frozen_bottom: Some(options.reads_top),
                subtracks: {
                    let mut subtracks = Vec::with_capacity(4);
                    if embedded_reference {
                        subtracks.push(TrackSubtrack {
                            label: "Reference".to_owned(),
                            detail: None,
                            top: options.reference_top,
                            bottom: options.reference_top + reference_height,
                        });
                    }
                    if self.primary_alignment_filters().show_coverage {
                        let scale = self
                            .primary_alignment_filters()
                            .coverage_scale_maximum
                            .map_or_else(
                                || format!("auto {}×", layout.stats.maximum_coverage),
                                |maximum| {
                                    format!(
                                        "max {}× · fixed {maximum}×",
                                        layout.stats.maximum_coverage
                                    )
                                },
                            );
                        subtracks.push(TrackSubtrack {
                            label: "Coverage".to_owned(),
                            detail: Some(scale),
                            top: options.coverage_top,
                            bottom: options.coverage_top + options.coverage_height,
                        });
                    }
                    if show_junctions {
                        subtracks.push(TrackSubtrack {
                            label: "Sashimi".to_owned(),
                            detail: Some(sashimi_track_detail(
                                &layout.stats,
                                &query.junctions,
                                self.primary_alignment_filters().sashimi_scale_mode,
                                self.primary_alignment_filters().sashimi_fixed_scale_maximum,
                                self.primary_alignment_filters().sashimi_count_mode,
                            )),
                            top: options.junction_top,
                            bottom: options.junction_top + options.junction_height,
                        });
                    }
                    if show_modification_frequency {
                        subtracks.push(TrackSubtrack {
                            label: "Modifications".to_owned(),
                            detail: Some(format!(
                                "{} types · {} tagged reads · {} malformed · {} unavailable{}",
                                query.pileup.modifications.series.len(),
                                query
                                    .pileup
                                    .modifications
                                    .provenance
                                    .records_with_modification_tags,
                                query.pileup.modifications.provenance.malformed_records,
                                query.pileup.modifications.provenance.unavailable_records,
                                if query.pileup.modifications.provenance.dropped_series > 0 {
                                    " · capped"
                                } else {
                                    ""
                                }
                            )),
                            top: options.modification_top,
                            bottom: options.modification_top + options.modification_height,
                        });
                    }
                    if self.primary_alignment_filters().show_reads {
                        let mut read_details = Vec::new();
                        let overview =
                            self.panels.active().viewport.detail_level() == DetailLevel::Overview;
                        if overview {
                            read_details.push(alignment::overview_read_detail(
                                query.stats.records_eligible,
                                query.stats.cancelled,
                            ));
                        } else if query.stats.truncated {
                            read_details.push(format!(
                                "sampled {}/{}",
                                query.stats.records_emitted, query.stats.records_eligible
                            ));
                        }
                        if layout.stats.bisulfite_marks_omitted > 0 {
                            read_details.push(format!(
                                "{} bisulfite marks omitted at display cap",
                                layout.stats.bisulfite_marks_omitted
                            ));
                        }
                        if layout.stats.smrt_kinetics_marks_omitted > 0 {
                            read_details.push(format!(
                                "{} PacBio kinetic marks omitted at display cap",
                                layout.stats.smrt_kinetics_marks_omitted
                            ));
                        }
                        if layout.stats.ultima_quality_marks_omitted > 0 {
                            read_details.push(format!(
                                "{} Ultima quality marks omitted at display cap",
                                layout.stats.ultima_quality_marks_omitted
                            ));
                        }
                        if layout.stats.sbx_tail_marks_omitted > 0 {
                            read_details.push(format!(
                                "{} SBX simplex-tail marks omitted at display cap",
                                layout.stats.sbx_tail_marks_omitted
                            ));
                        }
                        if matches!(
                            self.primary_alignment_filters().alignment_color_mode,
                            AlignmentColorMode::SmrtKinetics(_)
                        ) {
                            if layout.stats.smrt_kinetics_unavailable_records > 0 {
                                read_details.push(format!(
                                    "{} reads missing the selected PacBio kinetic array or sequence",
                                    layout.stats.smrt_kinetics_unavailable_records
                                ));
                            }
                            if layout.stats.smrt_kinetics_malformed_records > 0 {
                                read_details.push(format!(
                                    "{} malformed PacBio kinetic arrays",
                                    layout.stats.smrt_kinetics_malformed_records
                                ));
                            }
                        }
                        if matches!(
                            self.primary_alignment_filters().alignment_color_mode,
                            AlignmentColorMode::Ultima(_)
                        ) {
                            if layout.stats.ultima_unavailable_records > 0 {
                                read_details.push(format!(
                                    "{} reads missing Ultima sequence, qualities, or tp",
                                    layout.stats.ultima_unavailable_records
                                ));
                            }
                            if layout.stats.ultima_malformed_records > 0 {
                                read_details.push(format!(
                                    "{} reads with malformed Ultima evidence",
                                    layout.stats.ultima_malformed_records
                                ));
                            }
                            if layout.stats.ultima_unavailable_events > 0 {
                                read_details.push(format!(
                                    "{} indels shown without Ultima quality evidence",
                                    layout.stats.ultima_unavailable_events
                                ));
                            }
                        }
                        if matches!(
                            self.primary_alignment_filters().alignment_color_mode,
                            AlignmentColorMode::Sbx(_)
                        ) {
                            if layout.stats.sbx_unavailable_records > 0 {
                                read_details.push(format!(
                                    "{} reads missing usable SBX sequence/qualities",
                                    layout.stats.sbx_unavailable_records
                                ));
                            }
                            if layout.stats.sbx_unavailable_insertions > 0 {
                                read_details.push(format!(
                                    "{} SBX insertions shown without quality classification",
                                    layout.stats.sbx_unavailable_insertions
                                ));
                            }
                        }
                        if matches!(
                            self.primary_alignment_filters().alignment_color_mode,
                            AlignmentColorMode::Bisulfite(_)
                        ) && query.reference.is_none()
                        {
                            read_details.push(
                                "bisulfite conversion unavailable: load a matching reference FASTA"
                                    .to_owned(),
                            );
                        }
                        subtracks.push(TrackSubtrack {
                            label: "Reads".to_owned(),
                            detail: (!read_details.is_empty()).then(|| read_details.join(" · ")),
                            top: options.reads_top,
                            bottom,
                        });
                    }
                    subtracks
                },
            };
            self.panels.active_mut().track_frames.push(alignment_frame);
            top = bottom;
        } else if self.primary_alignment_source().is_some() {
            if let Some(track) = self.alignment_tracks.first_mut() {
                track.projection_mut(self.panels.active().id).cached_layout = None;
            }
            if self.primary_alignment_visible() {
                let collapsed = self.primary_alignment_collapsed();
                let natural_bottom = top + 26.0;
                let bottom = if collapsed {
                    natural_bottom
                } else {
                    alignment_frame_bottom(
                        top,
                        natural_bottom,
                        self.shared_primary_alignment_height,
                    )
                };
                self.panels.active_mut().track_frames.push(TrackFrame {
                    key: TrackFrameKey::Alignment(primary_alignment_id),
                    name: primary_alignment_name,
                    kind: TrackKind::Alignment,
                    source_label: primary_source_label,
                    top,
                    bottom,
                    visible: true,
                    collapsed,
                    frozen_bottom: None,
                    subtracks: Vec::new(),
                });
                top = bottom;
            }
        }
        let selected_alignment_track = self.panels.active().selected_alignment_track;
        let selected_chain_read_name_hash = self
            .panels
            .active()
            .selected_chain
            .as_ref()
            .map(|chain| alignment_read_name_hash(&chain.read_name));
        let promoted_read = self.promoted_read;
        for track in self
            .alignment_tracks
            .iter_mut()
            .skip(1)
            .filter(|track| track.config.visible)
        {
            let track_id = track.config.id;
            let (layout, frame) = secondary_alignment_track_layout(
                self.panels.active_mut().id,
                track,
                self.panels.active_mut().viewport,
                &self.panels.active_mut().contig_name,
                top,
                (selected_alignment_track == Some(track_id))
                    .then_some(selected_chain_read_name_hash)
                    .flatten(),
                promoted_read
                    .filter(|(promoted_track, _)| *promoted_track == track_id)
                    .map(|(_, hash)| hash),
                shared_sashimi_raw,
                shared_sashimi_cpm,
                &junction_annotations.lookup,
            );
            if let Some(layout) = layout {
                // CPU semantic picking resolves the owning TrackId from the frame.
                // Keep GPU object IDs clear because they are only unique within
                // one alignment query, not across the complete workspace.
                display
                    .rectangles
                    .extend(
                        layout
                            .display_list
                            .rectangles
                            .iter()
                            .copied()
                            .map(|mut rectangle| {
                                rectangle.object_id = 0;
                                rectangle
                            }),
                    );
                display
                    .texts
                    .extend(layout.display_list.texts.iter().cloned());
            }
            top = frame.bottom;
            self.panels.active_mut().track_frames.push(frame);
        }
        let visible_configs = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.visible)
            .collect::<Vec<_>>();
        let mut rendered_overlay_groups = BTreeSet::new();
        for config in &visible_configs {
            let members = if config.kind == TrackKind::Quantitative
                && let Some(group) = config.quantitative.overlay_group
            {
                if !rendered_overlay_groups.insert(group) {
                    continue;
                }
                quantitative_overlay_members(config, &visible_configs)
            } else {
                vec![*config]
            };
            let collapsed = members.iter().all(|member| member.collapsed);
            let anchor_snapshot = self
                .panels
                .active()
                .track_snapshots
                .iter()
                .find(|snapshot| snapshot.id == config.id);
            let mut effective_copy_number_settings = config.copy_number.clone();
            effective_copy_number_settings.row_height = (effective_copy_number_settings.row_height
                * auxiliary_track_scale(self.primary_alignment_filters().alignment_density))
            .clamp(6.0, 40.0);
            let mut effective_mutation_settings = config.mutation.clone();
            effective_mutation_settings.row_height = (effective_mutation_settings.row_height
                * auxiliary_track_scale(self.primary_alignment_filters().alignment_density))
            .clamp(8.0, 40.0);
            let height = if collapsed {
                26.0
            } else if let Some(TrackSnapshot {
                data: TrackSnapshotData::CopyNumber(query),
                ..
            }) = anchor_snapshot
            {
                copy_number_track_height(
                    query.samples.len(),
                    effective_copy_number_settings.row_height,
                )
                .max(40.0)
            } else if let Some(TrackSnapshot {
                data: TrackSnapshotData::Mutations(query),
                ..
            }) = anchor_snapshot
            {
                mutation_track_height(query.samples.len(), effective_mutation_settings.row_height)
                    .max(40.0)
            } else {
                members
                    .iter()
                    .filter(|member| !member.collapsed)
                    .map(|member| {
                        (member.height
                            * auxiliary_track_scale(
                                self.primary_alignment_filters().alignment_density,
                            ))
                        .clamp(40.0, 1_200.0)
                    })
                    .reduce(f32::max)
                    .unwrap_or(40.0)
            };
            let bottom = top + height;
            let mut subtracks = quantitative_overlay_subtracks(&members, top, height);
            if !collapsed && tracks_overlap_viewport {
                for member in members.iter().filter(|member| !member.collapsed) {
                    let Some(snapshot) = self
                        .panels
                        .active()
                        .track_snapshots
                        .iter()
                        .find(|snapshot| snapshot.id == member.id)
                    else {
                        continue;
                    };
                    let options = TrackLayoutOptions {
                        top,
                        height,
                        selected_record: self
                            .panels
                            .active()
                            .selected_feature
                            .filter(|selection| selection.track_id == member.id)
                            .map(|selection| selection.record_index)
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_variant
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_gwas
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_quantitative
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_interaction
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_copy_number
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.segment_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_mutation
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            }),
                        object_id_base: 1_000_000_u32.saturating_add(
                            u32::try_from(member.id.get())
                                .unwrap_or(u32::MAX)
                                .saturating_mul(100_000),
                        ),
                        feature_display_mode: member.feature.display_mode,
                        ..TrackLayoutOptions::default()
                    };
                    let track_display = match &snapshot.data {
                        TrackSnapshotData::Features(records) => {
                            layout_features(records, self.panels.active().viewport, options).0
                        }
                        TrackSnapshotData::Variants(records) => {
                            layout_variants(
                                &records.records,
                                self.panels.active().viewport,
                                options,
                                &member.variant,
                            )
                            .0
                        }
                        TrackSnapshotData::Gwas(query) => {
                            if query.provenance.records_truncated
                                || query.provenance.source_rows_ignored > 0
                            {
                                subtracks.push(TrackSubtrack {
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
                                });
                            }
                            layout_gwas(
                                &query.records,
                                self.panels.active().viewport,
                                options,
                                &member.gwas,
                            )
                            .0
                        }
                        TrackSnapshotData::Quantitative(_) => {
                            let shared_range = (member.quantitative.scale_mode
                                == QuantitativeScaleMode::Shared)
                                .then(|| {
                                    shared_quantitative_range(
                                        self.tracks.tracks(),
                                        &self.panels.active().track_snapshots,
                                        self.panels.active().viewport,
                                        member.quantitative.scale_transform,
                                        member.quantitative.scale_group,
                                    )
                                })
                                .flatten();
                            quantitative_display_records(
                                member,
                                &self.panels.active().track_snapshots,
                                self.panels.active().viewport,
                            )
                            .map_or_else(
                                DisplayList::default,
                                |records| {
                                    layout_quantitative(
                                        &records,
                                        self.panels.active().viewport,
                                        options,
                                        &member.quantitative,
                                        shared_range,
                                    )
                                    .0
                                },
                            )
                        }
                        TrackSnapshotData::CopyNumber(query) => {
                            let shared_range = (member.quantitative.scale_mode
                                == QuantitativeScaleMode::Shared)
                                .then(|| {
                                    shared_copy_number_range(
                                        member,
                                        self.tracks.tracks(),
                                        &self.panels.active().track_snapshots,
                                    )
                                })
                                .flatten();
                            let layout = layout_copy_number(
                                query,
                                self.panels.active().viewport,
                                options,
                                &effective_copy_number_settings,
                                &member.quantitative,
                                shared_range,
                                self.panels.active().vertical_scroll_points
                                    + self.canvas_geometry.viewport.top(),
                                self.panels.active().vertical_scroll_points
                                    + self.canvas_geometry.viewport.bottom(),
                            );
                            if effective_copy_number_settings.show_sample_names {
                                subtracks.extend(layout.visible_rows.iter().filter_map(|row| {
                                    query.samples.get(row.sample_index).map(|sample| {
                                        TrackSubtrack {
                                            label: sample.clone(),
                                            detail: query
                                                .sample_sort_values
                                                .get(row.sample_index)
                                                .copied()
                                                .flatten()
                                                .map(|value| format!("{value:.3}")),
                                            top: row.top,
                                            bottom: row.bottom,
                                        }
                                    })
                                }));
                            }
                            layout.display_list
                        }
                        TrackSnapshotData::Mutations(query) => {
                            let layout = layout_mutations(
                                query,
                                self.panels.active().viewport,
                                options,
                                &effective_mutation_settings,
                                self.panels.active().vertical_scroll_points
                                    + self.canvas_geometry.viewport.top(),
                                self.panels.active().vertical_scroll_points
                                    + self.canvas_geometry.viewport.bottom(),
                            );
                            if effective_mutation_settings.show_sample_names {
                                subtracks.extend(layout.visible_rows.iter().filter_map(|row| {
                                    query.samples.get(row.sample_index).map(|sample| {
                                        TrackSubtrack {
                                            label: sample.clone(),
                                            detail: Some("MAF tumour sample".to_owned()),
                                            top: row.top,
                                            bottom: row.bottom,
                                        }
                                    })
                                }));
                            }
                            layout.display_list
                        }
                        TrackSnapshotData::Interactions(query) => {
                            layout_interactions(
                                &query.records,
                                &self.panels.active().contig_name,
                                self.panels.active().viewport,
                                options,
                                &member.interaction,
                            )
                            .0
                        }
                    };
                    display.rectangles.extend(track_display.rectangles);
                    display.texts.extend(track_display.texts);
                }
            }
            let overlay_group = config.quantitative.overlay_group;
            self.panels.active_mut().track_frames.push(TrackFrame {
                key: TrackFrameKey::Auxiliary(config.id),
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
                visible: true,
                collapsed,
                frozen_bottom: None,
                subtracks,
            });
            top = bottom;
        }
        top = fill_lone_alignment_track(
            &mut self.panels.active_mut().track_frames,
            top,
            self.canvas_geometry.viewport.bottom(),
        );
        let content_bottom = display
            .rectangles
            .iter()
            .map(|rectangle| rectangle.y + rectangle.height)
            .fold(top, f32::max)
            .max(self.canvas_geometry.viewport.bottom());
        let overview = ui::scrollbar_overview(
            &self.panels.active().track_frames,
            &display,
            self.canvas_geometry.viewport.top(),
            content_bottom,
            self.canvas_geometry.genomic.width(),
            128,
        );
        self.panels.active_mut().scroll_display_cache = Some(PanelScrollDisplayCache {
            scrollable: display,
            pinned: pinned_display,
            overview,
            content_bottom,
            scroll_clip_top,
            supports_fast_scroll,
        });
        let rebuilt = self.rebuild_active_panel_from_scroll_cache();
        debug_assert!(rebuilt);
    }

    fn rebuild_active_panel_from_scroll_cache(&mut self) -> bool {
        let trace_started = self.interaction_trace.is_active().then(Instant::now);
        if trace_started.is_some() {
            self.interaction_trace
                .record(InteractionTraceStage::ScrollDisplayStarted);
        }
        let Some((content_bottom, scroll_clip_top)) = self
            .panels
            .active()
            .scroll_display_cache
            .as_ref()
            .map(|cache| (cache.content_bottom, cache.scroll_clip_top))
        else {
            return false;
        };
        let maximum_scroll = (content_bottom - self.canvas_geometry.viewport.bottom()).max(0.0);
        let active_panel_id = self.panels.active().id;
        let density_drag_fraction = self
            .vertical_overview_drag
            .filter(|drag| drag.density_dragging && drag.panel_id == active_panel_id)
            .map(|drag| drag.virtual_scroll_fraction);
        let panel = self.panels.active_mut();
        panel.content_bottom_points = content_bottom;
        panel.vertical_scroll_points = density_drag_fraction.map_or_else(
            || panel.vertical_scroll_points.clamp(0.0, maximum_scroll),
            |fraction| fraction.clamp(0.0, 1.0) * maximum_scroll,
        );
        let scroll = self.panels.active().vertical_scroll_points;
        let mut display = std::mem::take(&mut self.panels.active_mut().display_list);
        let rectangle_capacity = display.rectangles.capacity();
        let text_capacity = display.texts.capacity();
        {
            let cache = self
                .panels
                .active()
                .scroll_display_cache
                .as_ref()
                .expect("scroll display cache was observed above");
            cache.scrollable.write_scrolled_and_clipped_vertical(
                scroll,
                scroll_clip_top,
                self.canvas_geometry.viewport.bottom(),
                &mut display,
            );
            cache.pinned.append_scrolled_and_clipped_vertical(
                0.0,
                self.canvas_geometry.viewport.top(),
                scroll_clip_top,
                &mut display,
            );
        }
        let panel_rect = self.active_panel_rect();
        display.translate_and_clip_horizontal(
            panel_rect.left(),
            panel_rect.left(),
            panel_rect.right(),
        );
        self.add_region_overlays(&mut display);
        let capacity_grew = display.rectangles.capacity() > rectangle_capacity
            || display.texts.capacity() > text_capacity;
        if let Some(started) = trace_started {
            self.interaction_trace.record_scroll_display(
                started.elapsed(),
                display.rectangles.len(),
                display.texts.len(),
                capacity_grew,
            );
        }
        self.panels.active_mut().display_list = display;
        true
    }

    pub(crate) fn refresh_active_panel_scroll_display(&mut self) -> bool {
        if self.display_dirty
            || !self
                .panels
                .active()
                .scroll_display_cache
                .as_ref()
                .is_some_and(|cache| cache.supports_fast_scroll)
            || !self.rebuild_active_panel_from_scroll_cache()
        {
            return false;
        }
        self.composite_display_list = composite_panel_display_lists(&self.panels);
        self.prepare_rectangle_renderer();
        true
    }

    pub(crate) fn alignment_track_name(&self) -> String {
        self.primary_alignment_name().to_owned()
    }

    pub(crate) fn add_region_overlays(&self, display: &mut DisplayList) {
        let viewport = self.panels.active().viewport.interval();
        let panel = self.active_panel_rect();
        let bases_per_pixel = self.panels.active().viewport.bases_per_pixel();
        for (region, x, right) in self
            .regions_of_interest
            .iter()
            .filter_map(|region| {
                region_overlay_horizontal_span(
                    region,
                    &self.panels.active().contig_name,
                    viewport,
                    bases_per_pixel,
                    panel.left(),
                )
                .map(|(x, right)| (region, x, right))
            })
            .take(MAX_RENDERED_REGIONS_OF_INTEREST)
        {
            let color = Rgba::new(
                f32::from(region.color[0]) / 255.0,
                f32::from(region.color[1]) / 255.0,
                f32::from(region.color[2]) / 255.0,
                (f32::from(region.color[3]) / 255.0).min(0.18),
            );
            display.rectangles.push(RectPrimitive {
                x,
                y: self.canvas_geometry.viewport.top(),
                width: (right - x).max(1.0),
                height: self.canvas_geometry.viewport.height(),
                color,
                object_id: 0,
            });
            for boundary in [x, right - 1.0] {
                display.rectangles.push(RectPrimitive {
                    x: boundary,
                    y: self.canvas_geometry.viewport.top(),
                    width: 1.0,
                    height: self.canvas_geometry.viewport.height(),
                    color: Rgba::new(color.red, color.green, color.blue, 0.72),
                    object_id: 0,
                });
            }
        }
    }

    pub(crate) fn apply_canvas_scroll(
        &mut self,
        display: &mut DisplayList,
        pinned_display: &mut DisplayList,
        minimum_bottom: f32,
        scroll_clip_top: f32,
    ) {
        let panel_rect = self.active_panel_rect();
        let active_panel_id = self.panels.active().id;
        let density_drag_fraction = self
            .vertical_overview_drag
            .filter(|drag| drag.density_dragging && drag.panel_id == active_panel_id)
            .map(|drag| drag.virtual_scroll_fraction);
        self.panels.active_mut().content_bottom_points = display
            .rectangles
            .iter()
            .map(|rectangle| rectangle.y + rectangle.height)
            .fold(minimum_bottom, f32::max)
            .max(self.canvas_geometry.viewport.bottom());
        let maximum_scroll = (self.panels.active().content_bottom_points
            - self.canvas_geometry.viewport.bottom())
        .max(0.0);
        let current_scroll = self.panels.active().vertical_scroll_points;
        self.panels.active_mut().vertical_scroll_points = density_drag_fraction.map_or_else(
            || current_scroll.clamp(0.0, maximum_scroll),
            |fraction| fraction.clamp(0.0, 1.0) * maximum_scroll,
        );
        display.scroll_and_clip_vertical(
            self.panels.active().vertical_scroll_points,
            scroll_clip_top,
            self.canvas_geometry.viewport.bottom(),
        );
        display.translate_and_clip_horizontal(
            panel_rect.left(),
            panel_rect.left(),
            panel_rect.right(),
        );
        pinned_display.scroll_and_clip_vertical(
            0.0,
            self.canvas_geometry.viewport.top(),
            scroll_clip_top,
        );
        pinned_display.translate_and_clip_horizontal(
            panel_rect.left(),
            panel_rect.left(),
            panel_rect.right(),
        );
        // Frozen lanes are appended last so their backgrounds remain above any
        // scrollable primitive that touches the clipping boundary.
        display.rectangles.append(&mut pinned_display.rectangles);
        display.texts.append(&mut pinned_display.texts);
    }

    pub(crate) fn rebuild_all_panel_displays(&mut self) {
        let active_id = self.panels.active().id;
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        self.shared_primary_alignment_height = None;
        for panel_id in &panel_ids {
            self.panels.activate(*panel_id);
            self.rebuild_genomic_display();
        }
        if self.panels.len() > 1
            && let Some(height) =
                maximum_alignment_frame_height(&self.panels, self.primary_alignment_track_id())
        {
            self.shared_primary_alignment_height = Some(height);
            for panel_id in &panel_ids {
                self.panels.activate(*panel_id);
                self.rebuild_genomic_display();
            }
        }
        self.panels.activate(active_id);

        self.composite_display_list = composite_panel_display_lists(&self.panels);
    }

    pub(crate) fn rebuild_display_list(&mut self) {
        if !self.display_dirty {
            return;
        }
        if self.synthetic {
            let top = self.canvas_geometry.viewport.top();
            let mut display =
                if self.primary_alignment_visible() && !self.primary_alignment_collapsed() {
                    DisplayList::synthetic_coverage_and_reads_at(
                        self.panels.active().viewport.pixel_width() as f32,
                        100,
                        u32::try_from(self.primary_alignment_limit()).unwrap_or(u32::MAX),
                        top + 30.0,
                    )
                } else {
                    DisplayList::default()
                };
            let bottom = if self.primary_alignment_visible() && !self.primary_alignment_collapsed()
            {
                top + 730.0
            } else if self.primary_alignment_visible() {
                top + 26.0
            } else {
                top
            };
            self.panels.active_mut().track_frames = if self.primary_alignment_visible() {
                vec![TrackFrame {
                    key: TrackFrameKey::Alignment(SYNTHETIC_ALIGNMENT_TRACK_ID),
                    name: "Synthetic reads".to_owned(),
                    kind: TrackKind::Alignment,
                    source_label: Some("Synthetic GPU workload".to_owned()),
                    top,
                    bottom,
                    visible: true,
                    collapsed: self.primary_alignment_collapsed(),
                    frozen_bottom: None,
                    subtracks: if self.primary_alignment_collapsed() {
                        Vec::new()
                    } else {
                        vec![
                            TrackSubtrack {
                                label: "Coverage".to_owned(),
                                detail: None,
                                top: top + 30.0,
                                bottom: top + 90.0,
                            },
                            TrackSubtrack {
                                label: "Reads".to_owned(),
                                detail: None,
                                top: top + 100.0,
                                bottom,
                            },
                        ]
                    },
                }]
            } else {
                Vec::new()
            };
            self.apply_canvas_scroll(
                &mut display,
                &mut DisplayList::default(),
                bottom,
                self.canvas_geometry.viewport.top(),
            );
            self.panels.active_mut().display_list = display;
            self.composite_display_list = self.panels.active().display_list.clone();
        } else {
            self.rebuild_all_panel_displays();
        }
        self.prepare_rectangle_renderer();
        self.display_dirty = false;
    }

    fn prepare_rectangle_renderer(&mut self) {
        let trace_started = self.interaction_trace.is_active().then(Instant::now);
        if trace_started.is_some() {
            self.interaction_trace
                .record(InteractionTraceStage::RendererPrepareStarted);
        }
        let logical_height = self.surface_config.height as f32 / self.window.scale_factor() as f32;
        let logical_width = self.surface_config.width as f32 / self.window.scale_factor() as f32;
        self.rectangle_renderer.prepare(
            &self.device,
            &self.queue,
            [logical_width, logical_height],
            &self.composite_display_list,
            (self.panels.active().selected_alignment_track
                == Some(self.primary_alignment_track_id()))
            .then_some(self.panels.active().selected_record)
            .flatten()
            .and_then(|index| u32::try_from(index + 1).ok()),
            self.panels
                .active()
                .hovered_record
                .filter(|(track_id, _)| *track_id == self.primary_alignment_track_id())
                .and_then(|(_, index)| u32::try_from(index + 1).ok()),
        );
        if let Some(started) = trace_started {
            let stats = self.rectangle_renderer.last_prepare_stats();
            self.interaction_trace.record_renderer_prepare(
                started.elapsed(),
                stats.total_instances,
                stats.uploaded_bytes,
                stats.buffer_reallocated,
                stats.cpu_capacity_grew,
            );
        }
        self.display_upload_pending_metrics = true;
    }
}
