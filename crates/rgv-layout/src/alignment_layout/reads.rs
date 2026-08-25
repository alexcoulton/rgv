#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_lines)]
pub(crate) fn add_reads(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    viewport: Viewport,
    options: &AlignmentLayoutOptions,
    rows: &AlignmentRowLayout,
) -> ReadRenderStats {
    let batch = &query.batch;
    let mut stats = ReadRenderStats::default();
    let mut event_label_budget = 2_000_usize;
    let mut modification_budget = 50_000_usize;
    let mut bisulfite_budget = BisulfiteRenderBudget::new(MAX_RENDERED_BISULFITE_MARKS);
    let mut smrt_kinetics_budget = SmrtKineticsRenderBudget::new(MAX_RENDERED_SMRT_KINETICS_MARKS);
    let mut sbx_budget = SbxRenderBudget::new(MAX_RENDERED_SBX_TAIL_MARKS);
    let mut ultima_budget = UltimaRenderBudget::new(MAX_RENDERED_ULTIMA_QUALITY_MARKS);
    for index in 0..batch.len() {
        if batch.ends[index] <= viewport.interval().start()
            || batch.starts[index] >= viewport.interval().end()
        {
            continue;
        }
        let Some(row) = rows.rows[index] else {
            stats.hidden += 1;
            continue;
        };
        let smrt_mode = match options.color_mode {
            AlignmentColorMode::SmrtKinetics(mode) => Some(mode),
            _ => None,
        };
        let smrt_tag =
            smrt_mode.map(|mode| smrt_kinetics_tag(mode, batch.flags[index].is_reverse()));
        let smrt_field = smrt_tag.and_then(|tag| match batch.auxiliary_field(index, tag) {
            Ok(Some(field)) => Some(field),
            Ok(None) => {
                stats.smrt_kinetics_unavailable_records += 1;
                None
            }
            Err(_) => {
                stats.smrt_kinetics_malformed_records += 1;
                None
            }
        });
        let smrt_series = match (smrt_tag, smrt_field.as_ref(), batch.sequence(index)) {
            (Some(tag), Some(field), Some(SequenceView::Present { bases, .. })) => {
                if let Ok(series) = decode_smrt_kinetics(tag, &field.value, bases.len()) {
                    Some(series)
                } else {
                    stats.smrt_kinetics_malformed_records += 1;
                    None
                }
            }
            (Some(_), Some(_), _) => {
                stats.smrt_kinetics_unavailable_records += 1;
                None
            }
            _ => None,
        };
        let sbx_state = match (options.color_mode, batch.sequence(index)) {
            (
                AlignmentColorMode::Sbx(sbx_options),
                Some(SequenceView::Present {
                    bases,
                    qualities: Some(qualities),
                }),
            ) => {
                if let Ok(tails) = sbx_tail_bounds(
                    batch.flags[index],
                    bases,
                    qualities,
                    sbx_options.simplex_tail_quality_maximum,
                ) {
                    Some(SbxRenderState {
                        options: sbx_options,
                        tails,
                    })
                } else {
                    stats.sbx_unavailable_records += 1;
                    None
                }
            }
            (AlignmentColorMode::Sbx(_), _) => {
                stats.sbx_unavailable_records += 1;
                None
            }
            _ => None,
        };
        let ultima_analysis = match options.color_mode {
            AlignmentColorMode::Ultima(ultima_options) => {
                let read_group = query.read_group(index);
                let maximum_homopolymer_length = ultima_options
                    .maximum_homopolymer_length_override
                    .or_else(|| read_group.and_then(|group| group.maximum_homopolymer_length));
                let tag_orientation = read_group
                    .and_then(|group| group.ultima_tag_orientation)
                    .unwrap_or(UltimaTagOrientation::Reference);
                if let Ok(record) = query.materialize_record(index) {
                    match analyze_ultima_record_with_tag_orientation(
                        &record,
                        query.reference.as_ref(),
                        ultima_options.quality_mode,
                        maximum_homopolymer_length,
                        tag_orientation,
                    ) {
                        Ok(analysis) => {
                            stats.ultima_unavailable_events = stats
                                .ultima_unavailable_events
                                .saturating_add(analysis.summary.unavailable_events);
                            Some(analysis)
                        }
                        Err(error) if ultima_record_evidence_is_unavailable(&error) => {
                            stats.ultima_unavailable_records += 1;
                            None
                        }
                        Err(_) => {
                            stats.ultima_malformed_records += 1;
                            None
                        }
                    }
                } else {
                    stats.ultima_malformed_records += 1;
                    None
                }
            }
            _ => None,
        };
        let y = options.reads_top + row as f32 * options.row_height;
        let representative = rows.representative(index);
        let object_id = u32::try_from(representative + 1).unwrap_or(u32::MAX);
        if representative == index {
            add_mate_connector(
                display_list,
                query,
                index,
                viewport,
                y,
                options.read_height,
                object_id,
            );
        }
        let first_read_primitive = display_list.rectangles.len();
        add_cigar(
            display_list,
            query,
            index,
            viewport,
            y,
            options.read_height,
            object_id,
            options,
            &mut event_label_budget,
            &mut modification_budget,
            &mut bisulfite_budget,
            smrt_series,
            &mut smrt_kinetics_budget,
            sbx_state,
            &mut sbx_budget,
            ultima_analysis.as_ref(),
            &mut ultima_budget,
        );
        add_read_direction_terminal(
            display_list,
            first_read_primitive,
            viewport,
            batch.starts[index],
            batch.ends[index],
            batch.flags[index].is_reverse(),
            y,
            options.read_height,
            object_id,
        );
        if options
            .selected_chain_read_name_hash
            .is_some_and(|selected| {
                query
                    .batch
                    .read_name(index)
                    .is_some_and(|name| alignment_read_name_hash(name) == selected)
            })
        {
            let highlighted_span = sbx_state
                .filter(|state| state.options.simplex_tail_mode == SbxSimplexTailMode::Hide)
                .and_then(|state| {
                    sbx_retained_reference_span(
                        batch.starts[index],
                        batch.cigar(index)?,
                        state.tails,
                    )
                })
                .or_else(|| {
                    sbx_state
                        .is_none_or(|state| {
                            state.options.simplex_tail_mode != SbxSimplexTailMode::Hide
                        })
                        .then_some((batch.starts[index], batch.ends[index]))
                });
            if let Some((start, end)) = highlighted_span {
                add_chain_highlight(display_list, start, end, viewport, y, options.read_height);
            }
        }
        stats.rendered += 1;
    }
    stats.bisulfite_marks_omitted = bisulfite_budget.omitted;
    stats.smrt_kinetics_marks_omitted = smrt_kinetics_budget.omitted;
    stats.sbx_tail_marks_omitted = sbx_budget.omitted;
    stats.sbx_unavailable_insertions = sbx_budget.unavailable_insertions;
    stats.ultima_quality_marks_omitted = ultima_budget.omitted;
    stats
}

#[allow(clippy::too_many_arguments)]
fn add_read_direction_terminal(
    display_list: &mut DisplayList,
    first_primitive: usize,
    viewport: Viewport,
    alignment_start: u64,
    alignment_end: u64,
    reverse: bool,
    y: f32,
    height: f32,
    object_id: u32,
) {
    let interval = viewport.interval();
    let terminal_is_visible = if reverse {
        alignment_start >= interval.start() && alignment_start < interval.end()
    } else {
        alignment_end > interval.start() && alignment_end <= interval.end()
    };
    if !terminal_is_visible {
        return;
    }

    let body_height_minimum = (height - 1.7).max(0.5);
    let mut terminal = None::<f32>;
    for primitive in &display_list.rectangles[first_primitive..] {
        if record_object_id(primitive.object_id) != object_id
            || primitive.height < body_height_minimum
            || primitive.y < y - 0.1
            || primitive.y + primitive.height > y + height + 0.1
        {
            continue;
        }
        let edge = if reverse {
            primitive.x
        } else {
            primitive.x + primitive.width
        };
        terminal = Some(terminal.map_or(edge, |current| {
            if reverse {
                current.min(edge)
            } else {
                current.max(edge)
            }
        }));
    }
    let Some(terminal) = terminal else {
        return;
    };

    let shape = if reverse {
        RectShape::DirectionalTerminalLeft
    } else {
        RectShape::DirectionalTerminalRight
    };
    for primitive in &mut display_list.rectangles[first_primitive..] {
        if record_object_id(primitive.object_id) != object_id
            || primitive.height < body_height_minimum
            || primitive.y < y - 0.1
            || primitive.y + primitive.height > y + height + 0.1
        {
            continue;
        }
        let edge = if reverse {
            primitive.x
        } else {
            primitive.x + primitive.width
        };
        if (edge - terminal).abs() <= 1.0 {
            primitive.shape = shape;
        }
    }
}

pub(crate) const fn ultima_record_evidence_is_unavailable(error: &UltimaEvidenceError) -> bool {
    matches!(
        error,
        UltimaEvidenceError::SequenceUnavailable
            | UltimaEvidenceError::QualitiesUnavailable
            | UltimaEvidenceError::MissingTp
    )
}
