#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn add_coverage(
    display_list: &mut DisplayList,
    pileup: &PileupSummary,
    reference: Option<&ReferenceSlice>,
    query_interval: rgv_core::GenomicInterval,
    viewport: Viewport,
    width: f32,
    options: &AlignmentLayoutOptions,
) -> u32 {
    if pileup.is_empty() {
        return 0;
    }
    let visible_depths = pileup.bins.iter().enumerate().filter(|(index, _)| {
        coverage_bin_interval(query_interval, pileup.len(), *index)
            .is_some_and(|interval| interval.overlaps(viewport.interval()))
    });
    let maximum = visible_depths
        .map(|(_, bin)| bin.depth)
        .max()
        .unwrap_or_default();
    if maximum == 0 || !options.show_coverage {
        return maximum;
    }
    let scale_maximum = options.coverage_scale_maximum.unwrap_or(maximum).max(1);
    for (index, bin) in pileup.bins.iter().copied().enumerate() {
        if bin.depth == 0 {
            continue;
        }
        let Some(bin_interval) = coverage_bin_interval(query_interval, pileup.len(), index) else {
            continue;
        };
        let Some(visible) = bin_interval.intersection(viewport.interval()) else {
            continue;
        };
        let x = viewport.pixel_at_genomic(visible.start()) as f32;
        let right = viewport.pixel_at_genomic(visible.end()) as f32;
        let fraction = (bin.depth as f32 / scale_maximum as f32).min(1.0);
        let height = (fraction.sqrt() * options.coverage_height).max(1.0);
        display_list.rectangles.push(RectPrimitive {
            x,
            y: options.coverage_top + options.coverage_height - height,
            width: (right - x).max(1.0).min(width),
            height,
            color: Rgba::new(0.48, 0.48, 0.48, 0.90),
            object_id: 0,
        });
        if bin_interval.len() == 1
            && let Some(reference_base) =
                reference.and_then(|reference| reference.base_at(bin_interval.start()))
        {
            add_allele_coverage(
                display_list,
                bin,
                reference_base,
                x,
                options.coverage_top + options.coverage_height,
                (right - x).max(1.0).min(width),
                height,
                options.minimum_allele_fraction,
                options.quality_weighted_alleles,
            );
        }
    }
    maximum
}

pub(crate) fn add_coverage_navigation_markers(
    display_list: &mut DisplayList,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
) {
    if options.show_coverage_center_line {
        display_list.rectangles.push(RectPrimitive {
            x: viewport.pixel_width() as f32 * 0.5,
            y: options.coverage_top,
            width: 1.0,
            height: options.coverage_height,
            color: Rgba::new(0.18, 0.18, 0.18, 0.24),
            object_id: 0,
        });
    }
    if let Some(position) = options
        .selected_pileup_position
        .filter(|position| viewport.interval().contains(*position))
    {
        let x = viewport.pixel_at_genomic(position) as f32;
        let base_width = (1.0 / viewport.bases_per_pixel()) as f32;
        display_list.rectangles.push(RectPrimitive {
            x,
            y: options.coverage_top,
            width: base_width.clamp(2.0, 12.0),
            height: options.coverage_height,
            color: Rgba::new(0.08, 0.08, 0.08, 0.32),
            object_id: 0,
        });
    }
}

#[allow(clippy::too_many_lines)]
pub(crate) fn add_junctions(
    display_list: &mut DisplayList,
    summary: &JunctionSummary,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
    annotations: Option<&JunctionAnnotationLookup>,
) -> (usize, usize, f64, bool) {
    if !options.show_junctions || summary.junctions.is_empty() {
        return (
            0,
            0,
            0.0,
            options.junction_count_mode == SashimiCountMode::Raw
                || summary.provenance.normalization_mapped_records.is_some(),
        );
    }
    let interval = viewport.interval();
    let candidate_count = summary
        .junctions
        .iter()
        .filter(|junction| junction.start < interval.end() && junction.end > interval.start())
        .count();
    let mut visible = summary
        .junctions
        .iter()
        .enumerate()
        .filter(|(_, junction)| {
            junction.start < interval.end()
                && junction.end > interval.start()
                && junction.depth >= options.minimum_junction_depth
        })
        .collect::<Vec<_>>();
    let visible_count = visible.len();
    if visible_count > MAX_RENDERED_JUNCTIONS {
        visible.sort_by_key(|(index, junction)| {
            (
                Reverse(junction.depth),
                junction.start,
                junction.end,
                *index,
            )
        });
        visible.truncate(MAX_RENDERED_JUNCTIONS);
        visible.sort_by_key(|(index, junction)| (junction.start, junction.end, *index));
    } else {
        visible.sort_by_key(|(index, junction)| (junction.start, junction.end, *index));
    }
    let normalization_available = options.junction_count_mode == SashimiCountMode::Raw
        || summary.provenance.normalization_mapped_records.is_some();
    let automatic_maximum = visible
        .iter()
        .map(|(_, junction)| junction_count_value(summary, junction, options.junction_count_mode))
        .fold(0.0_f64, f64::max);
    let scale_maximum = options
        .junction_scale_maximum
        .filter(|maximum| maximum.is_finite() && *maximum > 0.0)
        .unwrap_or(automatic_maximum.max(f64::EPSILON));
    let expanded_rows = if options.junction_display_mode == JunctionDisplayMode::Expanded {
        expanded_junction_rows(&visible)
    } else {
        vec![0; visible.len()]
    };
    let row_count = expanded_rows.iter().copied().max().unwrap_or(0) + 1;
    let width = viewport.pixel_width() as f32;
    let mut rendered_labels = 0_usize;
    for ((_, junction), row) in visible.iter().zip(expanded_rows) {
        let left = (viewport.pixel_at_genomic(junction.start) as f32).clamp(0.0, width);
        let right = (viewport.pixel_at_genomic(junction.end) as f32).clamp(0.0, width);
        if right <= left {
            continue;
        }
        let value = junction_count_value(summary, junction, options.junction_count_mode);
        let fraction = (value / scale_maximum).clamp(0.0, 1.0).sqrt() as f32;
        let (baseline, arc_height) =
            if options.junction_display_mode == JunctionDisplayMode::Expanded {
                let band_height = options.junction_height / row_count.max(1) as f32;
                let baseline = options.junction_top + (row + 1) as f32 * band_height;
                let arc_height = (band_height * (0.25 + 0.70 * fraction)).clamp(2.0, band_height);
                (baseline, arc_height)
            } else {
                let baseline = options.junction_top + options.junction_height;
                let arc_height =
                    (options.junction_height * fraction).clamp(3.0, options.junction_height);
                (baseline, arc_height)
            };
        let arc_top = baseline - arc_height;
        let selected = options.selected_junction == [junction.start, junction.end];
        let color = if selected {
            Rgba::new(0.08, 0.08, 0.10, 1.0)
        } else {
            junction_color(junction)
        };
        let thickness = if selected { 2.5 } else { 1.0 + fraction };
        for rectangle in [
            RectPrimitive {
                x: left,
                y: arc_top,
                width: thickness,
                height: arc_height,
                color,
                object_id: 0,
            },
            RectPrimitive {
                x: left,
                y: arc_top,
                width: (right - left).max(thickness),
                height: thickness,
                color,
                object_id: 0,
            },
            RectPrimitive {
                x: (right - thickness).max(left),
                y: arc_top,
                width: thickness,
                height: arc_height,
                color,
                object_id: 0,
            },
        ] {
            display_list.rectangles.push(rectangle);
        }
        if let Some(annotations) = annotations {
            let status = annotations.status(junction.start, junction.end);
            let marker_color = match status {
                JunctionAnnotationStatus::Annotated => Rgba::new(0.16, 0.58, 0.28, 1.0),
                JunctionAnnotationStatus::Novel => Rgba::new(0.92, 0.48, 0.10, 1.0),
                JunctionAnnotationStatus::Unknown => Rgba::new(0.46, 0.48, 0.52, 0.92),
            };
            display_list.rectangles.push(RectPrimitive {
                x: ((left + right) * 0.5 - 2.5).clamp(0.0, (width - 5.0).max(0.0)),
                y: (arc_top - 2.5).max(options.junction_top),
                width: 5.0,
                height: 5.0,
                color: marker_color,
                object_id: 0,
            });
        }
        if options.show_junction_labels
            && rendered_labels < MAX_RENDERED_JUNCTION_LABELS
            && right - left >= 30.0
        {
            display_list.texts.push(TextPrimitive {
                x: (left + right) * 0.5,
                y: (arc_top - 5.5).max(options.junction_top + 5.5),
                text: junction_count_label(summary, junction, options.junction_count_mode),
                font_size: 10.0,
                color: Rgba::new(0.10, 0.10, 0.12, 0.94),
            });
            rendered_labels = rendered_labels.saturating_add(1);
        }
    }
    (
        visible.len(),
        candidate_count.saturating_sub(visible.len()),
        automatic_maximum,
        normalization_available,
    )
}

/// Returns a junction count in the declared Sashimi units. If whole-file index metadata is not
/// available for CPM, the raw count is returned so evidence remains visible; callers must surface
/// the accompanying availability state rather than labelling it CPM.
pub fn junction_count_value(
    summary: &JunctionSummary,
    junction: &JunctionEvidence,
    mode: SashimiCountMode,
) -> f64 {
    match mode {
        SashimiCountMode::Raw => f64::from(junction.depth),
        SashimiCountMode::CountsPerMillion => summary
            .provenance
            .normalization_mapped_records
            .filter(|total| *total > 0)
            .map_or_else(
                || f64::from(junction.depth),
                |total| f64::from(junction.depth) * 1_000_000.0 / total as f64,
            ),
    }
}

pub(crate) fn junction_count_label(
    summary: &JunctionSummary,
    junction: &JunctionEvidence,
    mode: SashimiCountMode,
) -> String {
    match (mode, summary.provenance.normalization_mapped_records) {
        (SashimiCountMode::CountsPerMillion, Some(total)) if total > 0 => {
            let value = junction_count_value(summary, junction, mode);
            if value < 10.0 {
                format!("{value:.1} CPM")
            } else {
                format!("{value:.0} CPM")
            }
        }
        (SashimiCountMode::CountsPerMillion, _) => format!("{} raw", junction.depth),
        (SashimiCountMode::Raw, _) => junction.depth.to_string(),
    }
}

pub(crate) fn expanded_junction_rows(visible: &[(usize, &JunctionEvidence)]) -> Vec<usize> {
    let mut row_ends = Vec::<u64>::new();
    visible
        .iter()
        .map(|(_, junction)| {
            if let Some((row, end)) = row_ends
                .iter_mut()
                .enumerate()
                .find(|(_, end)| **end <= junction.start)
            {
                *end = junction.end;
                row
            } else if row_ends.len() < MAX_EXPANDED_JUNCTION_ROWS {
                row_ends.push(junction.end);
                row_ends.len() - 1
            } else {
                let row = row_ends
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, end)| **end)
                    .map_or(0, |(row, _)| row);
                row_ends[row] = row_ends[row].max(junction.end);
                row
            }
        })
        .collect()
}

pub(crate) fn junction_color(junction: &JunctionEvidence) -> Rgba {
    match (
        junction.positive_depth > 0,
        junction.negative_depth > 0,
        junction.unknown_depth > 0,
    ) {
        (true, false, false) => Rgba::new(0.84, 0.30, 0.38, 0.88),
        (false, true, false) => Rgba::new(0.25, 0.45, 0.82, 0.88),
        (false, false, true) => Rgba::new(0.42, 0.45, 0.50, 0.82),
        _ => Rgba::new(0.58, 0.34, 0.70, 0.88),
    }
}

pub(crate) fn add_modification_frequency(
    display_list: &mut DisplayList,
    summary: &ModificationSummary,
    query_interval: rgv_core::GenomicInterval,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
) {
    if !options.show_modification_frequency || summary.bin_count() == 0 {
        return;
    }
    for index in 0..summary.bin_count() {
        let Some(interval) = summary.bin_interval(query_interval, index) else {
            continue;
        };
        let Some(visible) = interval.intersection(viewport.interval()) else {
            continue;
        };
        let dominant = summary
            .series
            .iter()
            .filter(|series| {
                !options
                    .hidden_modification_codes
                    .contains(&Some(series.key.code))
            })
            .filter_map(|series| {
                let bin = *series.bins.get(index)?;
                (bin.modified_calls > 0).then_some((series.key.code, bin))
            })
            .max_by(|left, right| {
                modification_bin_score(left.1).total_cmp(&modification_bin_score(right.1))
            });
        let Some((code, bin)) = dominant else {
            continue;
        };
        let x = viewport.pixel_at_genomic(visible.start()) as f32;
        let right = viewport.pixel_at_genomic(visible.end()) as f32;
        let width = (right - x).max(1.0);
        let fraction = modification_bin_score(bin).clamp(0.0, 1.0) as f32;
        if options.modified_base_color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified
            && bin.canonical_observations > 0
        {
            display_list.rectangles.push(RectPrimitive {
                x,
                y: options.modification_top,
                width,
                height: options.modification_height,
                color: Rgba::new(0.14, 0.44, 0.82, 0.20),
                object_id: 0,
            });
        }
        let color = match options.modified_base_color_mode {
            ModifiedBaseColorMode::ByType => modification_code_color(code),
            ModifiedBaseColorMode::Mono | ModifiedBaseColorMode::ModifiedVsUnmodified => {
                Rgba::new(0.90, 0.24, 0.92, 0.92)
            }
        };
        let height = (fraction * options.modification_height).max(1.0);
        display_list.rectangles.push(RectPrimitive {
            x,
            y: options.modification_top + options.modification_height - height,
            width,
            height,
            color,
            object_id: 0,
        });
    }
    if let Some(position) = options
        .selected_modification_position
        .filter(|position| viewport.interval().contains(*position))
    {
        display_list.rectangles.push(RectPrimitive {
            x: viewport.pixel_at_genomic(position) as f32,
            y: options.modification_top,
            width: ((1.0 / viewport.bases_per_pixel()) as f32).clamp(2.0, 12.0),
            height: options.modification_height,
            color: Rgba::new(0.08, 0.08, 0.08, 0.34),
            object_id: 0,
        });
    }
}

pub(crate) fn modification_bin_score(bin: ModificationBin) -> f64 {
    bin.raw_fraction()
        .or_else(|| {
            bin.mean_probability()
                .map(|probability| probability / f64::from(u8::MAX))
        })
        .unwrap_or(1.0)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_allele_coverage(
    display_list: &mut DisplayList,
    bin: PileupBin,
    reference_base: u8,
    x: f32,
    bottom: f32,
    width: f32,
    total_height: f32,
    minimum_fraction: f32,
    quality_weighted: bool,
) {
    let reference_base = reference_base.to_ascii_uppercase();
    let denominator = if quality_weighted {
        bin.quality_weighted.total() as f64
    } else {
        f64::from(bin.alleles.base_observations())
    };
    if denominator <= 0.0 {
        return;
    }
    let alternate_weight = [b'A', b'C', b'G', b'T', b'N']
        .into_iter()
        .filter(|base| *base != reference_base)
        .map(|base| {
            if quality_weighted {
                bin.quality_weighted.for_base(base) as f64
            } else {
                f64::from(bin.alleles.for_base(base))
            }
        })
        .sum::<f64>();
    if ((alternate_weight / denominator) as f32) < minimum_fraction.clamp(0.0, 1.0) {
        return;
    }

    let mut cursor = bottom;
    for base in [b'A', b'C', b'G', b'T', b'N'] {
        if base == reference_base {
            continue;
        }
        let support = if quality_weighted {
            bin.quality_weighted.for_base(base) as f64
        } else {
            f64::from(bin.alleles.for_base(base))
        };
        if support <= 0.0 {
            continue;
        }
        let height = (support / denominator) as f32 * total_height;
        cursor -= height;
        display_list.rectangles.push(RectPrimitive {
            x,
            y: cursor,
            width,
            height: height.max(1.0).min(total_height),
            color: base_color(base),
            object_id: 0,
        });
    }
}

pub(crate) fn coverage_bin_interval(
    query_interval: rgv_core::GenomicInterval,
    bins: usize,
    index: usize,
) -> Option<rgv_core::GenomicInterval> {
    if bins == 0 || index >= bins {
        return None;
    }
    let query_length = u128::from(query_interval.len());
    let bins = bins as u128;
    let start_offset = (index as u128 * query_length) / bins;
    let end_offset = ((index as u128 + 1) * query_length).div_ceil(bins);
    let start = query_interval
        .start()
        .saturating_add(u64::try_from(start_offset).ok()?);
    let end = query_interval
        .start()
        .saturating_add(u64::try_from(end_offset).ok()?)
        .min(query_interval.end());
    rgv_core::GenomicInterval::new(query_interval.contig(), start, end).ok()
}

pub(crate) fn add_spans(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
    rows: &AlignmentRowLayout,
) -> (usize, usize) {
    let batch = &query.batch;
    let mut rendered = 0;
    let mut hidden = 0;
    for index in 0..batch.len() {
        let (x, width) = clipped_geometry(batch.starts[index], batch.ends[index], viewport);
        if width <= 0.0 {
            continue;
        }
        let Some(row) = rows.rows[index] else {
            hidden += 1;
            continue;
        };
        let representative = rows.representative(index);
        add_read_body(
            display_list,
            x,
            options.reads_top + row as f32 * options.row_height,
            width.max(1.0),
            options.read_height,
            alignment_color(
                query,
                index,
                options.color_mode,
                options.pair_length_bounds,
                0.78,
            ),
            u32::try_from(representative + 1).unwrap_or(u32::MAX),
        );
        if options
            .selected_chain_read_name_hash
            .is_some_and(|selected| {
                batch
                    .read_name(index)
                    .is_some_and(|name| alignment_read_name_hash(name) == selected)
            })
        {
            add_chain_highlight(
                display_list,
                batch.starts[index],
                batch.ends[index],
                viewport,
                options.reads_top + row as f32 * options.row_height,
                options.read_height,
            );
        }
        rendered += 1;
    }
    (rendered, hidden)
}
