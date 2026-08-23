#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_bisulfite_marks(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    context: BisulfiteContext,
    reference_start: u64,
    read_start: usize,
    length: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    shade_by_quality: bool,
    budget: &mut BisulfiteRenderBudget,
) {
    let Some(SequenceView::Present { bases, qualities }) = query.batch.sequence(index) else {
        return;
    };
    for offset in 0..length {
        let position = reference_start.saturating_add(offset as u64);
        if !viewport.interval().contains(position) {
            continue;
        }
        let Some((base, quality)) = oriented_base(
            query.batch.flags[index],
            bases,
            qualities,
            read_start.saturating_add(offset),
        ) else {
            continue;
        };
        let Some(observation) = bisulfite_observation(query, index, context, position, base) else {
            continue;
        };
        if !budget.take() {
            continue;
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        let alpha = if shade_by_quality {
            quality.map_or(0.78, |value| {
                (0.40 + f32::from(value.min(40)) / 40.0 * 0.58).clamp(0.40, 0.98)
            })
        } else {
            0.98
        };
        display_list.rectangles.push(RectPrimitive {
            x,
            y,
            width: width.max(1.25),
            height,
            color: with_alpha(bisulfite_color(observation), alpha),
            object_id: read_detail_object_id(object_id),
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_smrt_kinetics_marks(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    series: SmrtKineticsSeries<'_>,
    reference_start: u64,
    read_start: usize,
    length: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    budget: &mut SmrtKineticsRenderBudget,
) {
    for offset in 0..length {
        let position = reference_start.saturating_add(offset as u64);
        if !viewport.interval().contains(position) {
            continue;
        }
        let Some(value) = series.value_at_aligned_read_position(
            query.batch.flags[index],
            read_start.saturating_add(offset),
        ) else {
            continue;
        };
        if !budget.take() {
            continue;
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        display_list.rectangles.push(RectPrimitive {
            x,
            y,
            width: width.max(1.25),
            height,
            color: smrt_kinetics_color(value),
            object_id: read_detail_object_id(object_id),
        });
    }
}

pub(crate) fn sbx_visible_read_range(
    state: Option<SbxRenderState>,
    read_start: usize,
    length: usize,
) -> Option<(usize, usize)> {
    let read_end = read_start.checked_add(length)?;
    let Some(state) = state else {
        return (read_start < read_end).then_some((read_start, read_end));
    };
    if state.options.simplex_tail_mode != SbxSimplexTailMode::Hide {
        return (read_start < read_end).then_some((read_start, read_end));
    }
    let visible_start = read_start.max(state.tails.retained_start);
    let visible_end = read_end.min(state.tails.retained_end);
    (visible_start < visible_end).then_some((visible_start, visible_end))
}

pub(crate) fn sbx_hides_read_position(state: Option<SbxRenderState>, read_position: usize) -> bool {
    state.is_some_and(|state| {
        state.options.simplex_tail_mode == SbxSimplexTailMode::Hide
            && state.tails.is_tail(read_position)
    })
}

pub(crate) fn sbx_retained_reference_span(
    alignment_start: u64,
    cigar: &[CigarOp],
    tails: SbxTailBounds,
) -> Option<(u64, u64)> {
    if tails.all_tail() {
        return None;
    }
    let start = reference_position_for_read_position(alignment_start, cigar, tails.retained_start)?;
    let final_read_position = tails.retained_end.checked_sub(1)?;
    let end = reference_position_for_read_position(alignment_start, cigar, final_read_position)?
        .saturating_add(1);
    (start < end).then_some((start, end))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_sbx_tail_marks(
    display_list: &mut DisplayList,
    reference_start: u64,
    read_start: usize,
    length: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    state: SbxRenderState,
    budget: &mut SbxRenderBudget,
) {
    let read_end = read_start.saturating_add(length);
    for (tail_start, tail_end) in [
        (0, state.tails.retained_start),
        (state.tails.retained_end, state.tails.read_length),
    ] {
        let visible_start = read_start.max(tail_start);
        let visible_end = read_end.min(tail_end);
        if visible_start >= visible_end {
            continue;
        }
        let genomic_start = reference_start.saturating_add((visible_start - read_start) as u64);
        let genomic_end = reference_start.saturating_add((visible_end - read_start) as u64);
        let (x, width) = clipped_geometry(genomic_start, genomic_end, viewport);
        if width > 0.0 && budget.take() {
            display_list.rectangles.push(RectPrimitive {
                x,
                y,
                width: width.max(1.0),
                height,
                color: sbx_simplex_tail_color(),
                object_id: read_detail_object_id(object_id),
            });
        }
    }
}

pub(crate) fn sbx_insertion_color(
    query: &AlignmentQuery,
    index: usize,
    enabled: bool,
    state: Option<SbxRenderState>,
    insertion_start: usize,
    insertion_length: usize,
    budget: &mut SbxRenderBudget,
) -> Rgba {
    let concordant = Rgba::new(0.86, 0.32, 0.82, 0.96);
    if !enabled {
        return concordant;
    }
    let Some(state) = state else {
        budget.unavailable_insertions = budget.unavailable_insertions.saturating_add(1);
        return concordant;
    };
    let Some(SequenceView::Present {
        bases,
        qualities: Some(qualities),
    }) = query.batch.sequence(index)
    else {
        budget.unavailable_insertions = budget.unavailable_insertions.saturating_add(1);
        return concordant;
    };
    match classify_sbx_insertion(
        query.batch.flags[index],
        bases,
        qualities,
        insertion_start,
        insertion_length,
        state.options.discordant_quality_maximum,
    ) {
        Ok(SbxInsertionState::Concordant) => concordant,
        Ok(SbxInsertionState::PartlyDiscordant) => Rgba::new(0.55, 0.55, 0.55, 0.98),
        Ok(SbxInsertionState::FullyDiscordant) => Rgba::new(0.71, 0.71, 0.71, 0.98),
        Err(_) => {
            budget.unavailable_insertions = budget.unavailable_insertions.saturating_add(1);
            concordant
        }
    }
}

pub(crate) const fn sbx_simplex_tail_color() -> Rgba {
    Rgba::new(0.59, 0.20, 0.78, 0.28)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn add_aligned_bases(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    cigar_kind: CigarOpKind,
    reference_start: u64,
    read_start: usize,
    length: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    color_mode: AlignmentColorMode,
    pair_length_bounds: TemplateLengthBounds,
    base_display_mode: ReadBaseDisplayMode,
    shade_by_quality: bool,
    bisulfite_budget: &mut BisulfiteRenderBudget,
    smrt_series: Option<SmrtKineticsSeries<'_>>,
    smrt_kinetics_budget: &mut SmrtKineticsRenderBudget,
    sbx_state: Option<SbxRenderState>,
    sbx_budget: &mut SbxRenderBudget,
) {
    let Some(sequence) = query.batch.sequence(index) else {
        add_match_segment(
            display_list,
            query,
            index,
            cigar_kind,
            reference_start,
            reference_start.saturating_add(length as u64),
            viewport,
            y,
            height,
            object_id,
            color_mode,
            pair_length_bounds,
        );
        return;
    };
    let SequenceView::Present { bases, qualities } = sequence else {
        add_match_segment(
            display_list,
            query,
            index,
            cigar_kind,
            reference_start,
            reference_start.saturating_add(length as u64),
            viewport,
            y,
            height,
            object_id,
            color_mode,
            pair_length_bounds,
        );
        return;
    };

    if let Some((visible_start, visible_end)) =
        sbx_visible_read_range(sbx_state, read_start, length)
    {
        let genomic_start = reference_start.saturating_add((visible_start - read_start) as u64);
        let genomic_end = reference_start.saturating_add((visible_end - read_start) as u64);
        let (segment_x, segment_width) = clipped_geometry(genomic_start, genomic_end, viewport);
        if segment_width > 0.0 {
            add_read_body(
                display_list,
                segment_x,
                y,
                segment_width.max(1.0),
                height,
                alignment_color(query, index, color_mode, pair_length_bounds, 0.88),
                object_id,
            );
        }
    }

    let highlights_sbx_tails = sbx_state
        .is_some_and(|state| state.options.simplex_tail_mode == SbxSimplexTailMode::Highlight);
    if base_display_mode == ReadBaseDisplayMode::Hidden
        && !matches!(
            color_mode,
            AlignmentColorMode::Bisulfite(_) | AlignmentColorMode::SmrtKinetics(_)
        )
        && !highlights_sbx_tails
    {
        return;
    }

    for offset in 0..length {
        let position = reference_start.saturating_add(offset as u64);
        if !viewport.interval().contains(position) {
            continue;
        }
        let aligned_read_position = read_start.saturating_add(offset);
        if sbx_hides_read_position(sbx_state, aligned_read_position) {
            continue;
        }
        let Some((base, quality)) = oriented_base(
            query.batch.flags[index],
            bases,
            qualities,
            aligned_read_position,
        ) else {
            continue;
        };
        let reference_base = query
            .reference
            .as_ref()
            .and_then(|value| value.base_at(position));
        let mismatch = match cigar_kind {
            CigarOpKind::SequenceMismatch => true,
            CigarOpKind::Match => {
                reference_base.is_some_and(|reference| !base.eq_ignore_ascii_case(&reference))
            }
            _ => false,
        };
        let bisulfite = match color_mode {
            AlignmentColorMode::Bisulfite(context) => {
                bisulfite_observation(query, index, context, position, base)
            }
            _ => None,
        };
        let smrt_kinetics = smrt_series.and_then(|series| {
            series.value_at_aligned_read_position(query.batch.flags[index], aligned_read_position)
        });
        let sbx_tail = highlights_sbx_tails
            && sbx_state.is_some_and(|state| state.tails.is_tail(aligned_read_position));
        if bisulfite.is_none()
            && smrt_kinetics.is_none()
            && !sbx_tail
            && (base_display_mode == ReadBaseDisplayMode::Hidden
                || (base_display_mode == ReadBaseDisplayMode::Mismatches && !mismatch))
        {
            continue;
        }
        if bisulfite.is_some() && !bisulfite_budget.take() {
            continue;
        }
        if smrt_kinetics.is_some() && !smrt_kinetics_budget.take() {
            continue;
        }
        if sbx_tail && !sbx_budget.take() {
            continue;
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        let quality_alpha = if shade_by_quality {
            quality.map_or(0.78, |value| {
                (0.40 + f32::from(value.min(40)) / 40.0 * 0.58).clamp(0.40, 0.98)
            })
        } else {
            0.98
        };
        let color = if sbx_tail {
            sbx_simplex_tail_color()
        } else {
            smrt_kinetics.map_or_else(
                || {
                    with_alpha(
                        bisulfite.map_or_else(|| base_color(base), bisulfite_color),
                        quality_alpha,
                    )
                },
                smrt_kinetics_color,
            )
        };
        display_list.rectangles.push(RectPrimitive {
            x,
            y,
            width: width.max(1.0),
            height,
            color,
            object_id: read_detail_object_id(object_id),
        });
        if width >= 7.0 {
            add_base_glyph(
                display_list,
                base,
                x,
                y + 1.0,
                width,
                (height - 2.0).max(5.0),
                if bisulfite.is_some() || smrt_kinetics.is_some() || sbx_tail {
                    fill_label_color(color)
                } else {
                    read_base_label_color(base)
                },
            );
        }
    }
}

pub(crate) const fn bisulfite_color(observation: BisulfiteObservation) -> Rgba {
    match observation {
        BisulfiteObservation::Protected => Rgba::new(0.86, 0.12, 0.12, 1.0),
        BisulfiteObservation::Converted => Rgba::new(0.12, 0.34, 0.88, 1.0),
    }
}
