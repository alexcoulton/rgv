use super::*;

pub(crate) fn snapshot_record_count(snapshot: &TrackSnapshot) -> usize {
    match &snapshot.data {
        TrackSnapshotData::Features(records) => records.len(),
        TrackSnapshotData::Variants(query) => query.records.len(),
        TrackSnapshotData::Quantitative(query) => query.records.len(),
        TrackSnapshotData::CopyNumber(query) => query.segments.len(),
        TrackSnapshotData::Mutations(query) => query.records.len(),
        TrackSnapshotData::Interactions(query) => query.records.len(),
        TrackSnapshotData::Gwas(query) => query.records.len(),
    }
}

pub(crate) fn pinned_gene_layout_maximum_rows(height: f32) -> usize {
    ((height / 20.0).floor() as usize).clamp(1, TrackLayoutOptions::default().maximum_rows)
}

#[cfg(test)]
pub(crate) fn feature_layout_row_count(
    maximum_rows: usize,
    display_mode: FeatureDisplayMode,
) -> usize {
    match display_mode {
        FeatureDisplayMode::Collapsed => 1,
        FeatureDisplayMode::Squished => maximum_rows.saturating_mul(2).max(1),
        FeatureDisplayMode::Expanded => maximum_rows.max(1),
    }
}

pub(crate) fn quantitative_overlay_members<'a>(
    anchor: &'a TrackConfig,
    tracks: &[&'a TrackConfig],
) -> Vec<&'a TrackConfig> {
    let Some(group) = anchor.quantitative.overlay_group else {
        return vec![anchor];
    };
    tracks
        .iter()
        .copied()
        .filter(|track| {
            track.kind == TrackKind::Quantitative && track.quantitative.overlay_group == Some(group)
        })
        .collect()
}

pub(crate) fn quantitative_overlay_subtracks(
    members: &[&TrackConfig],
    top: f32,
    height: f32,
) -> Vec<TrackSubtrack> {
    if members.len() < 2 {
        return Vec::new();
    }
    let available_rows =
        (((height - 22.0).max(0.0) / 14.0).floor() as usize).clamp(1, MAX_OVERLAY_GUTTER_MEMBERS);
    let truncated = members.len() > available_rows;
    let show_omitted_summary = truncated && available_rows > 1;
    let member_rows = if show_omitted_summary {
        available_rows.saturating_sub(1)
    } else {
        members.len().min(available_rows)
    };
    let mut subtracks = members
        .iter()
        .take(member_rows)
        .enumerate()
        .map(|(index, member)| TrackSubtrack {
            label: member.name.clone(),
            detail: member
                .quantitative
                .overlay_group
                .map(|group| format!("overlay {group}")),
            top: top + 22.0 + index as f32 * 14.0,
            bottom: top + 36.0 + index as f32 * 14.0,
            collapsed: !member.visible || member.collapsed,
            composite_member: Some(member.id),
        })
        .collect::<Vec<_>>();
    if show_omitted_summary {
        let omitted = members.len().saturating_sub(member_rows);
        let index = subtracks.len();
        subtracks.push(TrackSubtrack {
            label: format!("+{omitted} more tracks"),
            detail: Some("Open the Tracks menu to inspect every overlay member".to_owned()),
            top: top + 22.0 + index as f32 * 14.0,
            bottom: top + 36.0 + index as f32 * 14.0,
            collapsed: false,
            composite_member: None,
        });
    }
    subtracks
}

pub(crate) fn normalize_quantitative_overlay_settings(
    controls: &mut [(TrackId, QuantitativeTrackSettings)],
    edited_track: Option<TrackId>,
) {
    let edited_group_transform = edited_track.and_then(|edited_track| {
        controls
            .iter()
            .find(|(track_id, _)| *track_id == edited_track)
            .and_then(|(_, settings)| {
                settings
                    .overlay_group
                    .map(|group| (group, settings.scale_transform))
            })
    });
    for (_, settings) in controls {
        let Some(group) = settings.overlay_group else {
            continue;
        };
        settings.scale_mode = QuantitativeScaleMode::Shared;
        settings.scale_group = Some(group);
        if let Some((edited_group, transform)) = edited_group_transform
            && edited_group == group
        {
            settings.scale_transform = transform;
        }
    }
}

pub(crate) fn shared_copy_number_range(
    target: &TrackConfig,
    tracks: &[TrackConfig],
    snapshots: &[TrackSnapshot],
) -> Option<QuantitativeRange> {
    let target_query = snapshots
        .iter()
        .find(|snapshot| snapshot.id == target.id)
        .and_then(|snapshot| match &snapshot.data {
            TrackSnapshotData::CopyNumber(query) => Some(query.as_ref()),
            TrackSnapshotData::Features(_)
            | TrackSnapshotData::Variants(_)
            | TrackSnapshotData::Quantitative(_)
            | TrackSnapshotData::Mutations(_)
            | TrackSnapshotData::Interactions(_)
            | TrackSnapshotData::Gwas(_) => None,
        })?;
    let target_semantics = effective_copy_number_semantics(target, target_query);
    let baseline = match target_semantics {
        rgv_hts::CopyNumberValueSemantics::LogRatio => 0.0,
        rgv_hts::CopyNumberValueSemantics::Absolute => 2.0,
    };
    let mut minimum: f32 = baseline;
    let mut maximum: f32 = baseline;
    let mut found = false;
    for track in tracks.iter().filter(|track| {
        track.visible
            && track.kind == TrackKind::CopyNumber
            && track.quantitative.scale_mode == QuantitativeScaleMode::Shared
            && track.quantitative.scale_transform == target.quantitative.scale_transform
            && track.quantitative.scale_group == target.quantitative.scale_group
    }) {
        let Some(query) = snapshots
            .iter()
            .find(|snapshot| snapshot.id == track.id)
            .and_then(|snapshot| match &snapshot.data {
                TrackSnapshotData::CopyNumber(query) => Some(query.as_ref()),
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Variants(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
        else {
            continue;
        };
        if effective_copy_number_semantics(track, query) != target_semantics {
            continue;
        }
        for value in query
            .segments
            .iter()
            .map(|segment| segment.value)
            .filter(|value| value.is_finite())
        {
            minimum = minimum.min(value);
            maximum = maximum.max(value);
            found = true;
        }
    }
    found.then(|| {
        if minimum < maximum {
            QuantitativeRange { minimum, maximum }
        } else {
            let padding = minimum.abs().max(1.0) * 0.05;
            QuantitativeRange {
                minimum: minimum - padding,
                maximum: maximum + padding,
            }
        }
    })
}

pub(crate) fn effective_copy_number_semantics(
    track: &TrackConfig,
    query: &rgv_hts::CopyNumberQuery,
) -> rgv_hts::CopyNumberValueSemantics {
    match track.copy_number.value_mode {
        CopyNumberValueMode::Automatic => query.inferred_value_semantics,
        CopyNumberValueMode::LogRatio => rgv_hts::CopyNumberValueSemantics::LogRatio,
        CopyNumberValueMode::Absolute => rgv_hts::CopyNumberValueSemantics::Absolute,
    }
}

pub(crate) fn shared_quantitative_range(
    tracks: &[TrackConfig],
    snapshots: &[TrackSnapshot],
    viewport: Viewport,
    transform: QuantitativeScaleTransform,
    scale_group: Option<u32>,
) -> Option<QuantitativeRange> {
    tracks
        .iter()
        .filter(|track| {
            track.visible
                && track.kind == TrackKind::Quantitative
                && track.quantitative.scale_mode == QuantitativeScaleMode::Shared
                && track.quantitative.scale_transform == transform
                && track.quantitative.scale_group == scale_group
        })
        .filter_map(|track| {
            quantitative_display_records(track, snapshots, viewport).and_then(|records| {
                quantitative_data_range(&records, viewport, &track.quantitative)
            })
        })
        .reduce(QuantitativeRange::merge)
}

pub(crate) fn quantitative_track_y_at_position(
    track: &TrackConfig,
    tracks: &[TrackConfig],
    snapshots: &[TrackSnapshot],
    viewport: Viewport,
    position: u64,
    top: f32,
    height: f32,
) -> Option<f32> {
    let records = quantitative_display_records(track, snapshots, viewport)?;
    let value = quantitative_record_index_at_position(&records, position)
        .and_then(|index| records.get(index))?
        .value;
    let range = match track.quantitative.scale_mode {
        QuantitativeScaleMode::Automatic => {
            quantitative_data_range(&records, viewport, &track.quantitative)
        }
        QuantitativeScaleMode::Fixed => Some(QuantitativeRange {
            minimum: track.quantitative.fixed_minimum,
            maximum: track.quantitative.fixed_maximum,
        }),
        QuantitativeScaleMode::Shared => shared_quantitative_range(
            tracks,
            snapshots,
            viewport,
            track.quantitative.scale_transform,
            track.quantitative.scale_group,
        ),
    }
    .or_else(|| quantitative_data_range(&records, viewport, &track.quantitative))?;
    let transform = |value: f32| match track.quantitative.scale_transform {
        QuantitativeScaleTransform::Linear => value,
        QuantitativeScaleTransform::SignedLog10 => value.signum() * (1.0 + value.abs()).log10(),
    };
    let minimum = transform(range.minimum);
    let maximum = transform(range.maximum);
    let value = transform(value.clamp(range.minimum, range.maximum));
    Some(top + (maximum - value) / (maximum - minimum).max(f32::EPSILON) * height)
}

pub(crate) fn quantitative_display_records<'a>(
    track: &TrackConfig,
    snapshots: &'a [TrackSnapshot],
    viewport: Viewport,
) -> Option<Cow<'a, [rgv_hts::QuantitativeRecord]>> {
    let source = snapshots
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
    if track.quantitative.arithmetic == QuantitativeArithmetic::None {
        return Some(Cow::Borrowed(&source.records));
    }
    let operand_id = track.quantitative.operand_track?;
    let operand = snapshots
        .iter()
        .find(|snapshot| snapshot.id == operand_id)
        .and_then(|snapshot| match &snapshot.data {
            TrackSnapshotData::Quantitative(query) => Some(query.as_ref()),
            TrackSnapshotData::Features(_)
            | TrackSnapshotData::Variants(_)
            | TrackSnapshotData::CopyNumber(_)
            | TrackSnapshotData::Mutations(_)
            | TrackSnapshotData::Interactions(_)
            | TrackSnapshotData::Gwas(_) => None,
        })?;
    let interval = viewport.interval();
    Some(Cow::Owned(
        compose_quantitative_records(
            &source.records,
            &operand.records,
            track.quantitative.arithmetic,
            interval.start(),
            interval.end(),
            rgv_hts::MAX_QUANTITATIVE_RECORDS_PER_QUERY,
        )
        .records,
    ))
}

#[cfg(test)]
pub(crate) fn feature_record_index_at_position_and_row(
    records: &[rgv_hts::FeatureRecord],
    viewport: Viewport,
    position: u64,
    selected_row: usize,
    maximum_rows: usize,
    display_mode: FeatureDisplayMode,
) -> Option<usize> {
    let mut row_ends = vec![0_u64; maximum_rows.max(1)];
    records
        .iter()
        .enumerate()
        .filter_map(|(record_index, record)| {
            if record.end <= viewport.interval().start()
                || record.start >= viewport.interval().end()
            {
                return None;
            }
            let row = if display_mode == FeatureDisplayMode::Collapsed {
                0
            } else {
                row_ends.iter().position(|end| *end <= record.start)?
            };
            row_ends[row] = row_ends[row].max(record.end);
            (row == selected_row && record.start <= position && position < record.end)
                .then_some((record.end.saturating_sub(record.start), record_index))
        })
        .min()
        .map(|(_, record_index)| record_index)
}

pub(crate) fn feature_record_index_at_display_point(
    display: &DisplayList,
    records: &[rgv_hts::FeatureRecord],
    x: f32,
    y: f32,
    object_id_base: u32,
) -> Option<usize> {
    display
        .rectangles
        .iter()
        .filter_map(|rectangle| {
            if rectangle.object_id < object_id_base
                || x < rectangle.x
                || x >= rectangle.x + rectangle.width
                || y < rectangle.y
                || y >= rectangle.y + rectangle.height
            {
                return None;
            }
            let record_index = usize::try_from(rectangle.object_id - object_id_base).ok()?;
            let record = records.get(record_index)?;
            Some((record.end.saturating_sub(record.start), record_index))
        })
        .min()
        .map(|(_, record_index)| record_index)
}

pub(crate) fn quantitative_record_index_at_position(
    records: &[rgv_hts::QuantitativeRecord],
    position: u64,
) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .filter(|(_, record)| record.start <= position && position < record.end)
        .min_by(|(_, left), (_, right)| {
            left.end
                .saturating_sub(left.start)
                .cmp(&right.end.saturating_sub(right.start))
                .then_with(|| left.start.cmp(&right.start))
        })
        .map(|(index, _)| index)
}

pub(crate) fn interaction_record_index_at_position(
    records: &[rgv_hts::InteractionRecord],
    contig: &str,
    position: u64,
    tolerance: u64,
) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            let endpoint_distance = [&record.source, &record.target]
                .into_iter()
                .filter(|endpoint| endpoint.contig == contig)
                .map(|endpoint| {
                    genomic_distance_to_interval(position, endpoint.start, endpoint.end)
                })
                .min();
            let (distance_class, distance) =
                if endpoint_distance.is_some_and(|value| value <= tolerance) {
                    (0_u8, endpoint_distance.unwrap_or_default())
                } else if record.source.contig == contig && record.target.contig == contig {
                    let source = record
                        .source
                        .start
                        .saturating_add(record.source.end.saturating_sub(record.source.start) / 2);
                    let target = record
                        .target
                        .start
                        .saturating_add(record.target.end.saturating_sub(record.target.start) / 2);
                    if position < source.min(target) || position > source.max(target) {
                        return None;
                    }
                    (1, 0)
                } else {
                    return None;
                };
            let span = record
                .source
                .start
                .min(record.target.start)
                .abs_diff(record.source.end.max(record.target.end));
            Some((
                index,
                distance_class,
                distance,
                span,
                record.score.unwrap_or_default(),
            ))
        })
        .min_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
                .then_with(|| right.4.total_cmp(&left.4))
                .then_with(|| left.0.cmp(&right.0))
        })
        .map(|(index, _, _, _, _)| index)
}

pub(crate) fn effective_template_length_bounds_for_filters(
    filters: &SessionFilters,
    query: &AlignmentQuery,
) -> TemplateLengthBounds {
    let inferred = query.pairs.inferred_bounds.unwrap_or_default();
    TemplateLengthBounds {
        minimum: filters.minimum_template_length.or(inferred.minimum),
        maximum: filters.maximum_template_length.or(inferred.maximum),
    }
}

pub(crate) fn alignment_sort_position(
    mode: AlignmentSortMode,
    fixed_anchor: Option<u64>,
    viewport: Viewport,
) -> Option<u64> {
    if mode != AlignmentSortMode::Base {
        return None;
    }
    let interval = viewport.interval();
    fixed_anchor.or_else(|| {
        Some(
            interval
                .start()
                .saturating_add(interval.len() / 2)
                .min(interval.end().saturating_sub(1)),
        )
    })
}

pub(crate) fn mutation_record_index_at_position(
    records: &[rgv_hts::MutationRecord],
    sample_index: usize,
    position: u64,
) -> Option<usize> {
    records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.sample_index == sample_index && record.start <= position && position < record.end
        })
        .min_by_key(|(index, record)| (record.end.saturating_sub(record.start), *index))
        .map(|(index, _)| index)
}

pub(crate) fn genomic_distance_to_interval(position: u64, start: u64, end: u64) -> u64 {
    if position < start {
        start - position
    } else if position >= end {
        position.saturating_sub(end.saturating_sub(1))
    } else {
        0
    }
}

pub(crate) fn gwas_record_index_at_point(
    records: &[rgv_hts::GwasRecord],
    viewport: Viewport,
    local_x: f32,
    content_y: f32,
    top: f32,
    height: f32,
    settings: &GwasTrackSettings,
) -> Option<usize> {
    let tolerance = settings.point_size * 0.5 + 6.0;
    let tolerance_squared = tolerance * tolerance;
    records
        .iter()
        .enumerate()
        .filter_map(|(record_index, record)| {
            let point_x = viewport.pixel_at_genomic(record.position) as f32;
            let point_y = gwas_point_y(record.minus_log10_p, top, height, settings.maximum_score);
            let distance_squared = (local_x - point_x).powi(2) + (content_y - point_y).powi(2);
            (distance_squared <= tolerance_squared).then_some((distance_squared, record_index))
        })
        .min_by(|left, right| {
            left.0
                .total_cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
        })
        .map(|(_, record_index)| record_index)
}

pub(crate) fn sashimi_track_detail(
    stats: &AlignmentLayoutStats,
    summary: &rgv_hts::JunctionSummary,
    scale_mode: SashimiScaleMode,
    fixed_maximum: u32,
    count_mode: SashimiCountMode,
) -> String {
    let mut detail = format!("{} shown", stats.rendered_junctions);
    if stats.hidden_junctions > 0 {
        let _ = write!(detail, " · {} filtered/capped", stats.hidden_junctions);
    }
    match count_mode {
        SashimiCountMode::Raw => {
            let _ = write!(detail, " · max {} reads", stats.maximum_junction_depth);
        }
        SashimiCountMode::CountsPerMillion if stats.junction_normalization_available => {
            let _ = write!(
                detail,
                " · max {:.1} CPM / {} mapped reads",
                stats.maximum_junction_value,
                summary
                    .provenance
                    .normalization_mapped_records
                    .unwrap_or_default()
            );
        }
        SashimiCountMode::CountsPerMillion => {
            let _ = write!(
                detail,
                " · CPM unavailable (index has no mapped-read total; raw fallback max {})",
                stats.maximum_junction_depth
            );
        }
    }
    let scale = match scale_mode {
        SashimiScaleMode::Automatic => "auto scale".to_owned(),
        SashimiScaleMode::Fixed => format!("fixed scale {fixed_maximum}"),
        SashimiScaleMode::Shared if stats.junction_normalization_available => {
            "shared visible-track scale".to_owned()
        }
        SashimiScaleMode::Shared => "shared scale unavailable; per-track fallback".to_owned(),
    };
    let _ = write!(detail, " · {scale}");
    detail
}
