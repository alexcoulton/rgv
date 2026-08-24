use super::*;

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn secondary_alignment_track_layout(
    panel_id: PanelId,
    track: &mut AlignmentTrackController,
    viewport: Viewport,
    contig_name: &str,
    frame_top: f32,
    selected_read_name_hash: Option<u64>,
    promoted_read_name_hash: Option<u64>,
    shared_sashimi_raw: Option<f64>,
    shared_sashimi_cpm: Option<f64>,
    junction_annotations: &JunctionAnnotationLookup,
) -> (Option<Arc<AlignmentLayout>>, TrackFrame) {
    let config = track.config.clone();
    let lifecycle = track.lifecycle.clone();
    let active_filters = track.active_filters.clone();
    let frame_height = config.height.clamp(40.0, 1_200.0);
    let frame_bottom = if config.collapsed {
        frame_top + 26.0
    } else {
        frame_top + frame_height
    };
    let source_label = Some(config.source.redacted_label());
    if config.collapsed {
        return (
            None,
            TrackFrame {
                key: TrackFrameKey::Alignment(config.id),
                name: config.name,
                kind: TrackKind::Alignment,
                source_label,
                top: frame_top,
                bottom: frame_bottom,
                visible: true,
                collapsed: true,
                frozen_bottom: None,
                subtracks: Vec::new(),
            },
        );
    }
    let expected_interval = track
        .dictionary
        .as_ref()
        .and_then(|dictionary| interval_for_track(dictionary, contig_name, viewport.interval()));
    let projection = track.projection_mut(panel_id);
    let Some(query) = projection.current_query.clone().filter(|query| {
        expected_interval.is_some_and(|interval| retained_data_overlaps(query.interval, interval))
    }) else {
        let status = if let Some(message) = &projection.unavailable {
            format!("Unavailable: {message}")
        } else {
            match &lifecycle {
                AlignmentTrackLifecycle::Opening => "Opening…".to_owned(),
                AlignmentTrackLifecycle::Ready => "Loading locus…".to_owned(),
                AlignmentTrackLifecycle::Failed(message) => format!("Error: {message}"),
            }
        };
        return (
            None,
            TrackFrame {
                key: TrackFrameKey::Alignment(config.id),
                name: config.name,
                kind: TrackKind::Alignment,
                source_label,
                top: frame_top,
                bottom: frame_bottom,
                visible: true,
                collapsed: false,
                frozen_bottom: None,
                subtracks: vec![TrackSubtrack {
                    label: status,
                    detail: None,
                    top: frame_top + 22.0,
                    bottom: frame_bottom,
                }],
            },
        );
    };

    let filters = &config.filters;
    let density = filters.alignment_density;
    let secondary_scale = density.sqrt();
    let content_top = frame_top + 22.0;
    let mut lane_top = content_top;
    let mut options = AlignmentLayoutOptions {
        show_reference: false,
        reference_top: lane_top,
        show_coverage: filters.show_coverage,
        coverage_top: lane_top,
        coverage_height: (48.0 * secondary_scale).clamp(28.0, 84.0),
        coverage_scale_maximum: filters.coverage_scale_maximum,
        ..AlignmentLayoutOptions::default()
    };
    if options.show_coverage {
        lane_top += options.coverage_height + 6.0;
    }
    let show_junctions = filters.show_junctions && !query.junctions.junctions.is_empty();
    options.show_junctions = show_junctions;
    options.junction_top = lane_top;
    options.junction_height = match filters.junction_display_mode {
        JunctionDisplayMode::Collapsed => (34.0 * secondary_scale).clamp(24.0, 58.0),
        JunctionDisplayMode::Expanded => (104.0 * secondary_scale).clamp(72.0, 180.0),
    };
    options.minimum_junction_depth = filters.minimum_junction_depth;
    options.junction_display_mode = filters.junction_display_mode;
    options.junction_count_mode = filters.sashimi_count_mode;
    options.show_junction_labels = filters.show_junction_labels;
    options.junction_annotation_fingerprint = junction_annotations.fingerprint();
    options.junction_scale_maximum = match filters.sashimi_scale_mode {
        SashimiScaleMode::Automatic => None,
        SashimiScaleMode::Fixed => Some(f64::from(filters.sashimi_fixed_scale_maximum)),
        SashimiScaleMode::Shared => match filters.sashimi_count_mode {
            SashimiCountMode::Raw => shared_sashimi_raw,
            SashimiCountMode::CountsPerMillion => shared_sashimi_cpm,
        },
    };
    if show_junctions {
        lane_top += options.junction_height + 6.0;
    }
    let show_modification_frequency =
        filters.show_modification_frequency && viewport.detail_level() != DetailLevel::Overview;
    options.show_modification_frequency = show_modification_frequency;
    options.modification_top = lane_top;
    options.modification_height = (26.0 * secondary_scale).clamp(20.0, 40.0);
    if show_modification_frequency {
        lane_top += options.modification_height + 8.0;
    }
    options.show_reads = filters.show_reads;
    options.reads_top = lane_top + if filters.show_reads { 1.0 } else { 0.0 };
    options.row_height = (7.0 * density).clamp(2.5, MAX_ALIGNMENT_ROW_HEIGHT);
    options.read_height = (5.0 * density).clamp(1.5, (options.row_height - 1.0).max(1.5));
    options.color_mode = filters.alignment_color_mode;
    options.group_mode = filters.alignment_group_mode;
    options.group_order = filters.alignment_group_order;
    options.sort_mode = filters.alignment_sort_mode;
    options.sort_order = filters.alignment_sort_order;
    options.sort_position = alignment_sort_position(filters.alignment_sort_mode, viewport);
    options.promoted_read_name_hash = promoted_read_name_hash;
    options.selected_chain_read_name_hash = selected_read_name_hash.or(promoted_read_name_hash);
    options.view_as_pairs = filters.view_as_pairs;
    options.pair_length_bounds = effective_template_length_bounds_for_filters(filters, &query);
    options.minimum_allele_fraction = filters.minimum_allele_fraction;
    options.quality_weighted_alleles = filters.quality_weighted_alleles;
    options.show_coverage_center_line = filters.show_coverage_center_line;
    options.read_base_display_mode = filters.read_base_display_mode;
    options.shade_read_bases_by_quality = filters.shade_read_bases_by_quality;
    options.minimum_indel_size = filters.minimum_indel_size;
    options.minimum_event_label_size = filters.minimum_event_label_size;
    options.show_modified_bases = filters.show_modified_bases;
    options.minimum_modification_probability = filters.minimum_modification_probability;
    options.modified_base_color_mode = filters.modified_base_color_mode;
    for (slot, code) in options.hidden_modification_codes.iter_mut().zip(
        filters
            .hidden_modification_codes
            .iter()
            .filter_map(|value| parse_modification_code(value)),
    ) {
        *slot = Some(code);
    }
    let available_read_height = (frame_bottom - options.reads_top - 6.0).max(options.row_height);
    options.max_rows = if options.show_reads {
        (available_read_height / options.row_height)
            .floor()
            .clamp(1.0, 4_096.0) as usize
    } else {
        1
    };

    let rows_match = projection.alignment_rows.as_ref().is_some_and(|cached| {
        Arc::ptr_eq(&cached.query, &query)
            && cached.group_mode == options.group_mode
            && cached.group_order == options.group_order
            && cached.sort_mode == options.sort_mode
            && cached.sort_order == options.sort_order
            && cached.sort_position == options.sort_position
            && cached.promoted_read_name_hash == options.promoted_read_name_hash
            && cached.max_rows == options.max_rows
            && cached.view_as_pairs == options.view_as_pairs
            && cached.pair_length_bounds == options.pair_length_bounds
    });
    if !rows_match {
        let previous = projection.alignment_rows.as_ref().and_then(|cached| {
            (cached.group_mode == options.group_mode
                && cached.group_order == options.group_order
                && cached.sort_mode == options.sort_mode
                && cached.sort_order == options.sort_order
                && cached.sort_position == options.sort_position
                && cached.promoted_read_name_hash == options.promoted_read_name_hash
                && cached.max_rows == options.max_rows
                && cached.view_as_pairs == options.view_as_pairs
                && cached.pair_length_bounds == options.pair_length_bounds)
                .then_some(cached.layout.as_ref())
        });
        let layout = Arc::new(assign_alignment_rows(&query, options, previous));
        projection.alignment_rows = Some(CachedAlignmentRows {
            query: Arc::clone(&query),
            layout,
            group_mode: options.group_mode,
            group_order: options.group_order,
            sort_mode: options.sort_mode,
            sort_order: options.sort_order,
            sort_position: options.sort_position,
            promoted_read_name_hash: options.promoted_read_name_hash,
            max_rows: options.max_rows,
            view_as_pairs: options.view_as_pairs,
            pair_length_bounds: options.pair_length_bounds,
        });
        projection.cached_layout = None;
    }
    let key = alignment_layout_key(
        PanelTrackId::new(panel_id, config.id),
        Some(&config.source),
        viewport,
        query.interval,
        &active_filters,
        &options,
        frame_top,
    );
    let layout = if let Some((_, layout)) = projection
        .cached_layout
        .as_ref()
        .filter(|(cached_key, _)| *cached_key == key)
    {
        Arc::clone(layout)
    } else {
        let rows = projection
            .alignment_rows
            .as_ref()
            .expect("secondary alignment rows exist for a loaded query");
        let layout = Arc::new(layout_alignment_query_with_rows_and_annotations(
            &query,
            viewport,
            options,
            &rows.layout,
            Some(junction_annotations),
        ));
        projection.cached_layout = Some((key, Arc::clone(&layout)));
        layout
    };
    let mut subtracks = Vec::with_capacity(4);
    if options.show_coverage {
        subtracks.push(TrackSubtrack {
            label: "Coverage".to_owned(),
            detail: Some(filters.coverage_scale_maximum.map_or_else(
                || format!("auto {}×", layout.stats.maximum_coverage),
                |maximum| format!("max {}× · fixed {maximum}×", layout.stats.maximum_coverage),
            )),
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
                filters.sashimi_scale_mode,
                filters.sashimi_fixed_scale_maximum,
                filters.sashimi_count_mode,
            )),
            top: options.junction_top,
            bottom: options.junction_top + options.junction_height,
        });
    }
    if show_modification_frequency {
        subtracks.push(TrackSubtrack {
            label: "Modifications".to_owned(),
            detail: Some(format!("{} types", query.pileup.modifications.series.len())),
            top: options.modification_top,
            bottom: options.modification_top + options.modification_height,
        });
    }
    if options.show_reads {
        let overview = viewport.detail_level() == DetailLevel::Overview;
        let mut read_details = if overview {
            vec![overview_read_detail(
                query.stats.records_eligible,
                query.stats.cancelled,
            )]
        } else {
            vec![format!(
                "{} read{} rendered",
                layout.stats.rendered_alignments,
                if layout.stats.rendered_alignments == 1 {
                    ""
                } else {
                    "s"
                }
            )]
        };
        if query.stats.truncated && !overview {
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
            filters.alignment_color_mode,
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
        if matches!(filters.alignment_color_mode, AlignmentColorMode::Ultima(_)) {
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
        if matches!(filters.alignment_color_mode, AlignmentColorMode::Sbx(_)) {
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
            filters.alignment_color_mode,
            AlignmentColorMode::Bisulfite(_)
        ) && query.reference.is_none()
        {
            read_details.push(
                "bisulfite conversion unavailable: load a matching reference FASTA".to_owned(),
            );
        }
        subtracks.push(TrackSubtrack {
            label: "Reads".to_owned(),
            detail: (!read_details.is_empty()).then(|| read_details.join(" · ")),
            top: options.reads_top,
            bottom: frame_bottom,
        });
    }
    (
        Some(layout),
        TrackFrame {
            key: TrackFrameKey::Alignment(config.id),
            name: config.name,
            kind: TrackKind::Alignment,
            source_label,
            top: frame_top,
            bottom: frame_bottom,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks,
        },
    )
}

pub(crate) fn overview_read_detail(count: usize, cancelled: bool) -> String {
    if cancelled {
        if count == 0 {
            "overview query incomplete".to_owned()
        } else {
            format!("{count}+ found · overview incomplete")
        }
    } else if count == 0 {
        "no alignments in region".to_owned()
    } else {
        format!("{count} present · hidden at this zoom")
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn alignment_layout_key(
    owner: PanelTrackId,
    source: Option<&TrackSource>,
    viewport: Viewport,
    query_interval: GenomicInterval,
    filters: &ActiveFilters,
    options: &AlignmentLayoutOptions,
    track_height: f32,
) -> LayoutTileKey {
    let source_value = source.map_or(0, |source| match source {
        TrackSource::Local { path } => fnv64(path.as_bytes()),
        TrackSource::Remote { url } => fnv64(url.as_bytes()),
    });
    let mut layout_bytes = Vec::with_capacity(128);
    layout_bytes.extend_from_slice(&viewport.pixel_width().to_bits().to_le_bytes());
    layout_bytes.extend_from_slice(&viewport.exact_start().to_bits().to_le_bytes());
    layout_bytes.extend_from_slice(&viewport.span().to_bits().to_le_bytes());
    layout_bytes.extend_from_slice(&query_interval.contig().get().to_le_bytes());
    layout_bytes.extend_from_slice(&query_interval.start().to_le_bytes());
    layout_bytes.extend_from_slice(&query_interval.end().to_le_bytes());
    for value in [
        options.reference_top,
        options.coverage_top,
        options.coverage_height,
        options.junction_top,
        options.junction_height,
        options.reads_top,
        options.row_height,
        options.read_height,
        track_height,
    ] {
        layout_bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    layout_bytes.extend_from_slice(&options.max_rows.to_le_bytes());
    layout_bytes.push(options.group_order as u8);
    layout_bytes.push(options.sort_order as u8);
    layout_bytes.extend_from_slice(&options.sort_position.unwrap_or(u64::MAX).to_le_bytes());
    layout_bytes.push(u8::from(options.view_as_pairs));
    layout_bytes.extend_from_slice(
        &options
            .pair_length_bounds
            .minimum
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    layout_bytes.extend_from_slice(
        &options
            .pair_length_bounds
            .maximum
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    layout_bytes.extend_from_slice(&options.minimum_allele_fraction.to_bits().to_le_bytes());
    layout_bytes.push(u8::from(options.quality_weighted_alleles));
    layout_bytes.push(u8::from(options.show_reference));
    layout_bytes.push(u8::from(options.show_coverage));
    layout_bytes.push(u8::from(options.show_junctions));
    layout_bytes.push(u8::from(options.show_reads));
    layout_bytes.extend_from_slice(&options.minimum_junction_depth.to_le_bytes());
    layout_bytes.push(options.junction_display_mode as u8);
    layout_bytes.extend_from_slice(
        &options
            .junction_scale_maximum
            .unwrap_or(f64::NAN)
            .to_bits()
            .to_le_bytes(),
    );
    layout_bytes.push(options.junction_count_mode as u8);
    layout_bytes.push(u8::from(options.show_junction_labels));
    layout_bytes.extend_from_slice(&options.junction_annotation_fingerprint.to_le_bytes());
    layout_bytes.extend_from_slice(&options.selected_junction[0].to_le_bytes());
    layout_bytes.extend_from_slice(&options.selected_junction[1].to_le_bytes());
    layout_bytes.extend_from_slice(
        &options
            .coverage_scale_maximum
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    layout_bytes.push(u8::from(options.show_coverage_center_line));
    layout_bytes.push(options.read_base_display_mode as u8);
    layout_bytes.push(u8::from(options.shade_read_bases_by_quality));
    layout_bytes.extend_from_slice(&options.minimum_indel_size.to_le_bytes());
    layout_bytes.extend_from_slice(&options.minimum_event_label_size.to_le_bytes());
    layout_bytes.push(u8::from(options.show_modified_bases));
    layout_bytes.push(u8::from(options.show_modification_frequency));
    layout_bytes.extend_from_slice(&options.modification_top.to_bits().to_le_bytes());
    layout_bytes.extend_from_slice(&options.modification_height.to_bits().to_le_bytes());
    layout_bytes.extend_from_slice(
        &options
            .selected_modification_position
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    layout_bytes.extend_from_slice(
        &options
            .selected_chain_read_name_hash
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    layout_bytes.extend_from_slice(
        &options
            .promoted_read_name_hash
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    layout_bytes.push(options.minimum_modification_probability);
    layout_bytes.push(options.modified_base_color_mode as u8);
    for code in options.hidden_modification_codes {
        match code {
            None => layout_bytes.push(0),
            Some(ModificationCode::Letter(letter)) => {
                layout_bytes.push(1);
                layout_bytes.push(letter);
            }
            Some(ModificationCode::Chebi(code)) => {
                layout_bytes.push(2);
                layout_bytes.extend_from_slice(&code.to_le_bytes());
            }
        }
    }
    layout_bytes.extend_from_slice(
        &options
            .selected_pileup_position
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    let style_version = alignment_color_mode_fingerprint(options.color_mode)
        | alignment_group_mode_fingerprint(options.group_mode) << 24
        | alignment_sort_mode_fingerprint(options.sort_mode) << 48;
    LayoutTileKey {
        owner,
        source: SourceFingerprint::new(source_value),
        interval: viewport.interval(),
        detail: viewport.detail_level(),
        filter_hash: filters.alignment.fingerprint()
            ^ u64::from(filters.include_secondary)
            ^ (u64::from(filters.include_supplementary) << 1),
        layout_hash: fnv64(&layout_bytes),
        style_version,
    }
}

pub(crate) const fn alignment_color_mode_fingerprint(mode: AlignmentColorMode) -> u64 {
    match mode {
        AlignmentColorMode::None => 0,
        AlignmentColorMode::Strand => 1,
        AlignmentColorMode::MappingQuality => 2,
        AlignmentColorMode::ReadGroup => 3,
        AlignmentColorMode::Haplotype => 4,
        AlignmentColorMode::TemplateLength => 5,
        AlignmentColorMode::PairOrientation => 6,
        AlignmentColorMode::UnexpectedPair => 7,
        AlignmentColorMode::MateContig => 8,
        AlignmentColorMode::FirstOfPairStrand => 9,
        AlignmentColorMode::Bisulfite(context) => 10 + bisulfite_context_fingerprint(context),
        AlignmentColorMode::SmrtKinetics(mode) => 16 + smrt_kinetics_mode_fingerprint(mode),
        AlignmentColorMode::Ultima(options) => {
            let maximum = match options.maximum_homopolymer_length_override {
                Some(value) => value as u64,
                None => 0,
            };
            23 | (options.quality_mode as u64) << 8
                | maximum << 16
                | (options.maximum_homopolymer_length_override.is_some() as u64) << 40
        }
        AlignmentColorMode::Sbx(options) => {
            22 | (options.simplex_tail_mode as u64) << 8
                | (options.discordant_quality_maximum as u64) << 16
                | (options.simplex_tail_quality_maximum as u64) << 24
        }
        AlignmentColorMode::Tag(tag) => 32 | ((tag[0] as u64) << 8) | ((tag[1] as u64) << 16),
    }
}

pub(crate) const fn smrt_kinetics_mode_fingerprint(mode: SmrtKineticsMode) -> u64 {
    match mode {
        SmrtKineticsMode::SubreadIpd => 0,
        SmrtKineticsMode::SubreadPulseWidth => 1,
        SmrtKineticsMode::CcsForwardIpd => 2,
        SmrtKineticsMode::CcsForwardPulseWidth => 3,
        SmrtKineticsMode::CcsReverseIpd => 4,
        SmrtKineticsMode::CcsReversePulseWidth => 5,
    }
}

pub(crate) const fn bisulfite_context_fingerprint(context: BisulfiteContext) -> u64 {
    match context {
        BisulfiteContext::Cg => 0,
        BisulfiteContext::Chg => 1,
        BisulfiteContext::Chh => 2,
        BisulfiteContext::Hcg => 3,
        BisulfiteContext::Gch => 4,
        BisulfiteContext::Wcg => 5,
    }
}

pub(crate) const fn alignment_group_mode_fingerprint(mode: AlignmentGroupMode) -> u64 {
    match mode {
        AlignmentGroupMode::None => 0,
        AlignmentGroupMode::Strand => 1,
        AlignmentGroupMode::ReadGroup => 2,
        AlignmentGroupMode::Haplotype => 3,
        AlignmentGroupMode::Chimeric => 4,
        AlignmentGroupMode::Supplementary => 5,
        AlignmentGroupMode::LeftClip => 6,
        AlignmentGroupMode::RightClip => 7,
        AlignmentGroupMode::PairOrientation => 8,
        AlignmentGroupMode::UnexpectedPair => 9,
        AlignmentGroupMode::MateContig => 10,
        AlignmentGroupMode::ReadOrder => 11,
        AlignmentGroupMode::FirstOfPairStrand => 12,
        AlignmentGroupMode::Tag(tag) => 13 | ((tag[0] as u64) << 8) | ((tag[1] as u64) << 16),
    }
}

pub(crate) const fn alignment_sort_mode_fingerprint(mode: AlignmentSortMode) -> u64 {
    match mode {
        AlignmentSortMode::Start => 0,
        AlignmentSortMode::Strand => 1,
        AlignmentSortMode::FirstOfPairStrand => 2,
        AlignmentSortMode::MappingQuality => 3,
        AlignmentSortMode::ReadName => 4,
        AlignmentSortMode::ReadGroup => 5,
        AlignmentSortMode::Haplotype => 6,
        AlignmentSortMode::AlignedLength => 7,
        AlignmentSortMode::LeftClip => 8,
        AlignmentSortMode::RightClip => 9,
        AlignmentSortMode::TemplateLength => 10,
        AlignmentSortMode::PairOrientation => 11,
        AlignmentSortMode::MateContig => 12,
        AlignmentSortMode::PairStatus => 13,
        AlignmentSortMode::ReadOrder => 14,
        AlignmentSortMode::Base => 15,
        AlignmentSortMode::Tag(tag) => 16 | ((tag[0] as u64) << 8) | ((tag[1] as u64) << 16),
    }
}

pub(crate) fn parse_style_tag(value: &str) -> Option<[u8; 2]> {
    let bytes: [u8; 2] = value.as_bytes().try_into().ok()?;
    (bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_alphanumeric()).then_some(bytes)
}

pub(crate) fn persisted_style_tag(
    color: AlignmentColorMode,
    group: AlignmentGroupMode,
    sort: AlignmentSortMode,
) -> String {
    let tag = match (color, group, sort) {
        (AlignmentColorMode::Tag(tag), _, _)
        | (_, AlignmentGroupMode::Tag(tag), _)
        | (_, _, AlignmentSortMode::Tag(tag)) => tag,
        _ => *b"NM",
    };
    String::from_utf8_lossy(&tag).into_owned()
}
