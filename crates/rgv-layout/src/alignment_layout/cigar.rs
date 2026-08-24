#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn add_chain_highlight(
    display_list: &mut DisplayList,
    start: u64,
    end: u64,
    viewport: Viewport,
    y: f32,
    height: f32,
) {
    let (x, width) = clipped_geometry(start, end, viewport);
    if width <= 0.0 {
        return;
    }
    let color = Rgba::new(0.05, 0.66, 0.78, 1.0);
    let stroke = 1.5_f32.min(height * 0.35).max(1.0);
    for rectangle in [
        RectPrimitive {
            x,
            y: y - stroke,
            width,
            height: stroke,
            color,
            object_id: 0,
        },
        RectPrimitive {
            x,
            y: y + height,
            width,
            height: stroke,
            color,
            object_id: 0,
        },
        RectPrimitive {
            x: x - stroke,
            y: y - stroke,
            width: stroke,
            height: height + stroke * 2.0,
            color,
            object_id: 0,
        },
        RectPrimitive {
            x: x + width,
            y: y - stroke,
            width: stroke,
            height: height + stroke * 2.0,
            color,
            object_id: 0,
        },
    ] {
        display_list.rectangles.push(rectangle);
    }
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_cigar(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    options: &AlignmentLayoutOptions,
    event_label_budget: &mut usize,
    modification_budget: &mut usize,
    bisulfite_budget: &mut BisulfiteRenderBudget,
    smrt_series: Option<SmrtKineticsSeries<'_>>,
    smrt_kinetics_budget: &mut SmrtKineticsRenderBudget,
    sbx_state: Option<SbxRenderState>,
    sbx_budget: &mut SbxRenderBudget,
    ultima_analysis: Option<&UltimaRecordAnalysis>,
    ultima_budget: &mut UltimaRenderBudget,
) {
    let batch = &query.batch;
    let Some(cigar) = batch.cigar(index) else {
        return;
    };
    let mut reference_position = batch.starts[index];
    let mut read_position = 0_usize;
    if cigar.is_empty() {
        let (x, width) = clipped_geometry(batch.starts[index], batch.ends[index], viewport);
        add_read_body(
            display_list,
            x,
            y,
            width.max(1.0),
            height,
            alignment_color(
                query,
                index,
                options.color_mode,
                options.pair_length_bounds,
                0.92,
            ),
            object_id,
        );
        return;
    }
    for (operation_index, operation) in cigar.iter().enumerate() {
        let length = u64::from(operation.len);
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let end = reference_position.saturating_add(length);
                if viewport.detail_level() == DetailLevel::Bases {
                    add_aligned_bases(
                        display_list,
                        query,
                        index,
                        operation.kind,
                        reference_position,
                        read_position,
                        operation.len as usize,
                        viewport,
                        y,
                        height,
                        object_id,
                        options.color_mode,
                        options.pair_length_bounds,
                        options.read_base_display_mode,
                        options.shade_read_bases_by_quality,
                        bisulfite_budget,
                        smrt_series,
                        smrt_kinetics_budget,
                        sbx_state,
                        sbx_budget,
                    );
                } else {
                    if let Some((visible_start, visible_end)) =
                        sbx_visible_read_range(sbx_state, read_position, operation.len as usize)
                    {
                        let visible_reference_start = reference_position
                            .saturating_add((visible_start - read_position) as u64);
                        let visible_reference_end =
                            reference_position.saturating_add((visible_end - read_position) as u64);
                        add_match_segment(
                            display_list,
                            query,
                            index,
                            operation.kind,
                            visible_reference_start,
                            visible_reference_end,
                            viewport,
                            y,
                            height,
                            object_id,
                            options.color_mode,
                            options.pair_length_bounds,
                        );
                    }
                    if let AlignmentColorMode::Bisulfite(context) = options.color_mode {
                        add_bisulfite_marks(
                            display_list,
                            query,
                            index,
                            context,
                            reference_position,
                            read_position,
                            operation.len as usize,
                            viewport,
                            y,
                            height,
                            object_id,
                            options.shade_read_bases_by_quality,
                            bisulfite_budget,
                        );
                    }
                    if let Some(series) = smrt_series {
                        add_smrt_kinetics_marks(
                            display_list,
                            query,
                            index,
                            series,
                            reference_position,
                            read_position,
                            operation.len as usize,
                            viewport,
                            y,
                            height,
                            object_id,
                            smrt_kinetics_budget,
                        );
                    }
                    if sbx_state.is_some_and(|state| {
                        state.options.simplex_tail_mode == SbxSimplexTailMode::Highlight
                    }) {
                        add_sbx_tail_marks(
                            display_list,
                            reference_position,
                            read_position,
                            operation.len as usize,
                            viewport,
                            y,
                            height,
                            object_id,
                            sbx_state.expect("checked SBX state"),
                            sbx_budget,
                        );
                    }
                }
                reference_position = end;
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(length);
                let (x, width) = clipped_geometry(reference_position, end, viewport);
                let visible_event = operation.kind == CigarOpKind::ReferenceSkip
                    || operation.len >= options.minimum_indel_size;
                let visible_after_sbx_tail_hiding = sbx_state.is_none_or(|state| {
                    state.options.simplex_tail_mode != SbxSimplexTailMode::Hide
                        || (read_position > state.tails.retained_start
                            && read_position < state.tails.retained_end)
                });
                if visible_event && visible_after_sbx_tail_hiding && width > 0.0 {
                    display_list.rectangles.push(RectPrimitive {
                        x,
                        y: y + height * 0.48,
                        width: width.max(1.0),
                        height: 1.0,
                        color: if operation.kind == CigarOpKind::ReferenceSkip {
                            Rgba::new(0.26, 0.36, 0.52, 0.92)
                        } else {
                            Rgba::new(0.12, 0.12, 0.14, 0.96)
                        },
                        object_id: read_detail_object_id(object_id),
                    });
                    if width >= 18.0 && operation.len >= options.minimum_event_label_size {
                        let label = if operation.kind == CigarOpKind::ReferenceSkip {
                            format!("N{}", operation.len)
                        } else {
                            operation.len.to_string()
                        };
                        add_event_label(
                            display_list,
                            &label,
                            x + width * 0.5,
                            y + height * 0.5,
                            if operation.kind == CigarOpKind::ReferenceSkip {
                                Rgba::new(0.20, 0.31, 0.52, 1.0)
                            } else {
                                Rgba::new(0.10, 0.10, 0.12, 1.0)
                            },
                            object_id,
                            event_label_budget,
                        );
                    }
                    if operation.kind == CigarOpKind::Deletion
                        && let Some(quality) = ultima_analysis
                            .and_then(|analysis| analysis.event(operation_index))
                            .and_then(|event| event.quality)
                        && ultima_budget.take()
                    {
                        add_ultima_deletion_quality_mark(
                            display_list,
                            x,
                            width,
                            y,
                            height,
                            quality,
                            object_id,
                        );
                    }
                }
                reference_position = end;
            }
            CigarOpKind::Insertion => {
                let visible_range =
                    sbx_visible_read_range(sbx_state, read_position, operation.len as usize);
                let visible_length = visible_range
                    .map(|(start, end)| end.saturating_sub(start))
                    .unwrap_or_default();
                let insertion_color =
                    visible_range.map_or(insertion_event_color(0.96), |(start, end)| {
                        sbx_insertion_color(
                            query,
                            index,
                            matches!(options.color_mode, AlignmentColorMode::Sbx(_)),
                            sbx_state,
                            start,
                            end.saturating_sub(start),
                            sbx_budget,
                        )
                    });
                if visible_length >= options.minimum_indel_size as usize {
                    add_marker(
                        display_list,
                        viewport,
                        reference_position,
                        y,
                        height,
                        insertion_color,
                        object_id,
                    );
                    if let Some(quality) = ultima_analysis
                        .and_then(|analysis| analysis.event(operation_index))
                        .and_then(|event| event.quality)
                        && ultima_budget.take()
                    {
                        add_ultima_insertion_quality_mark(
                            display_list,
                            viewport,
                            reference_position,
                            y,
                            height,
                            quality,
                            object_id,
                        );
                    }
                }
                if visible_length >= options.minimum_indel_size as usize
                    && visible_length >= options.minimum_event_label_size as usize
                    && viewport.bases_per_pixel() <= 5.0
                {
                    let label = if visible_length == 1 {
                        "I".to_owned()
                    } else {
                        format!("I{visible_length}")
                    };
                    add_event_label(
                        display_list,
                        &label,
                        viewport.pixel_at_genomic(reference_position) as f32,
                        y + height * 0.5,
                        with_alpha(insertion_color, 1.0),
                        object_id,
                        event_label_budget,
                    );
                }
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::SoftClip => {
                let visible =
                    sbx_visible_read_range(sbx_state, read_position, operation.len as usize)
                        .is_some();
                if visible {
                    add_marker(
                        display_list,
                        viewport,
                        reference_position,
                        y,
                        height,
                        Rgba::new(0.62, 0.40, 0.86, 0.92),
                        object_id,
                    );
                }
                if visible
                    && operation.len >= options.minimum_event_label_size
                    && viewport.bases_per_pixel() <= 2.0
                {
                    add_event_label(
                        display_list,
                        "S",
                        viewport.pixel_at_genomic(reference_position) as f32,
                        y + height * 0.5,
                        Rgba::new(0.48, 0.26, 0.72, 1.0),
                        object_id,
                        event_label_budget,
                    );
                }
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::HardClip => add_marker(
                display_list,
                viewport,
                reference_position,
                y,
                height,
                Rgba::new(0.62, 0.40, 0.86, 0.92),
                object_id,
            ),
            CigarOpKind::Padding => add_marker(
                display_list,
                viewport,
                reference_position,
                y,
                height,
                Rgba::new(0.92, 0.76, 0.20, 0.90),
                object_id,
            ),
        }
    }
    if options.show_modified_bases {
        add_modified_base_calls(
            display_list,
            query,
            index,
            viewport,
            y,
            height,
            object_id,
            options,
            modification_budget,
            sbx_state,
        );
    }
}

pub(crate) fn add_mate_connector(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
) {
    let batch = &query.batch;
    let (Some(mate_contig), Some(mate_start)) =
        (batch.mate_contigs[index], batch.mate_starts[index])
    else {
        return;
    };
    if mate_contig != batch.contigs[index] {
        return;
    }
    let start = batch.ends[index].min(mate_start);
    let end = batch.ends[index].max(mate_start);
    let (x, width) = clipped_geometry(start, end, viewport);
    if width <= 0.0 {
        return;
    }
    display_list.rectangles.push(RectPrimitive {
        x,
        y: y + height * 0.5,
        width,
        height: 1.0,
        color: Rgba::new(0.58, 0.64, 0.74, 0.62),
        object_id,
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_modified_base_calls(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    options: &AlignmentLayoutOptions,
    budget: &mut usize,
    sbx_state: Option<SbxRenderState>,
) {
    let batch = &query.batch;
    let Some(SequenceView::Present { bases, .. }) = batch.sequence(index) else {
        return;
    };
    let Some(cigar) = batch.cigar(index) else {
        return;
    };
    let mm = batch
        .auxiliary_field(index, *b"MM")
        .ok()
        .flatten()
        .or_else(|| batch.auxiliary_field(index, *b"Mm").ok().flatten());
    let Some(mm) = mm else {
        return;
    };
    let ml = batch
        .auxiliary_field(index, *b"ML")
        .ok()
        .flatten()
        .or_else(|| batch.auxiliary_field(index, *b"Ml").ok().flatten());
    let Ok(calls) =
        decode_modified_bases_parts(bases, &mm.value, ml.as_ref().map(|field| &field.value))
    else {
        return;
    };
    let sequence_length = bases.len();
    for call in &calls {
        if *budget == 0 {
            break;
        }
        if (options.minimum_modification_probability > 0
            && call
                .probability
                .is_none_or(|probability| probability < options.minimum_modification_probability))
            || options.hidden_modification_codes.contains(&Some(call.code))
        {
            continue;
        }
        let aligned_position = if batch.flags[index].is_reverse() {
            let Some(position) = sequence_length.checked_sub(call.read_position.saturating_add(1))
            else {
                continue;
            };
            position
        } else {
            call.read_position
        };
        if sbx_hides_read_position(sbx_state, aligned_position) {
            continue;
        }
        let Some(reference_position) =
            reference_position_for_read_position(batch.starts[index], cigar, aligned_position)
        else {
            continue;
        };
        if !viewport.interval().contains(reference_position) {
            continue;
        }
        let (x, base_width) = clipped_geometry(
            reference_position,
            reference_position.saturating_add(1),
            viewport,
        );
        let alpha = call
            .probability
            .map_or(0.78, |value| 0.35 + f32::from(value) / 255.0 * 0.65);
        let mut color = match options.modified_base_color_mode {
            ModifiedBaseColorMode::ByType => modification_code_color(call.code),
            ModifiedBaseColorMode::Mono | ModifiedBaseColorMode::ModifiedVsUnmodified => {
                Rgba::new(0.90, 0.24, 0.92, 1.0)
            }
        };
        color.alpha = alpha;
        display_list.rectangles.push(RectPrimitive {
            x: x + base_width * 0.2,
            y: y - 2.0,
            width: (base_width * 0.6).clamp(2.0, 8.0),
            height: (height * 0.22).clamp(2.0, 4.0),
            color,
            object_id: read_detail_object_id(object_id),
        });
        *budget -= 1;
    }
    if options.modified_base_color_mode == ModifiedBaseColorMode::ModifiedVsUnmodified {
        add_unmodified_base_calls(
            display_list,
            &calls,
            bases,
            cigar,
            batch.starts[index],
            batch.flags[index],
            viewport,
            y,
            height,
            object_id,
            options,
            budget,
            sbx_state,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_unmodified_base_calls(
    display_list: &mut DisplayList,
    calls: &[rgv_hts::ModifiedBaseCall],
    bases: &[u8],
    cigar: &[CigarOp],
    alignment_start: u64,
    flags: AlignmentFlags,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    options: &AlignmentLayoutOptions,
    budget: &mut usize,
    sbx_state: Option<SbxRenderState>,
) {
    let mut groups = Vec::<(u8, ModificationCode, BTreeSet<usize>)>::new();
    for call in calls {
        if call.implicit_mode != ImplicitModificationMode::Canonical
            || options.hidden_modification_codes.contains(&Some(call.code))
        {
            continue;
        }
        if let Some((_, _, positions)) = groups
            .iter_mut()
            .find(|(base, code, _)| *base == call.canonical_base && *code == call.code)
        {
            positions.insert(call.read_position);
        } else {
            groups.push((
                call.canonical_base,
                call.code,
                BTreeSet::from([call.read_position]),
            ));
        }
    }
    for (canonical_base, _, called_positions) in groups {
        for (read_position, base) in bases.iter().copied().enumerate() {
            if *budget == 0 {
                return;
            }
            if base.to_ascii_uppercase() != canonical_base
                || called_positions.contains(&read_position)
            {
                continue;
            }
            let aligned_position = if flags.is_reverse() {
                let Some(position) = bases.len().checked_sub(read_position.saturating_add(1))
                else {
                    continue;
                };
                position
            } else {
                read_position
            };
            if sbx_hides_read_position(sbx_state, aligned_position) {
                continue;
            }
            let Some(reference_position) =
                reference_position_for_read_position(alignment_start, cigar, aligned_position)
            else {
                continue;
            };
            if !viewport.interval().contains(reference_position) {
                continue;
            }
            let (x, base_width) = clipped_geometry(
                reference_position,
                reference_position.saturating_add(1),
                viewport,
            );
            display_list.rectangles.push(RectPrimitive {
                x: x + base_width * 0.2,
                y: y - 2.0,
                width: (base_width * 0.6).clamp(2.0, 8.0),
                height: (height * 0.22).clamp(2.0, 4.0),
                color: Rgba::new(0.14, 0.44, 0.82, 0.72),
                object_id: read_detail_object_id(object_id),
            });
            *budget -= 1;
        }
    }
}

pub fn modification_code_color(code: ModificationCode) -> Rgba {
    match code {
        ModificationCode::Letter(b'm') => Rgba::new(0.86, 0.20, 0.58, 1.0),
        ModificationCode::Letter(b'h') => Rgba::new(0.12, 0.62, 0.70, 1.0),
        ModificationCode::Letter(b'a') => Rgba::new(0.92, 0.52, 0.10, 1.0),
        ModificationCode::Letter(b'f') => Rgba::new(0.48, 0.32, 0.80, 1.0),
        ModificationCode::Letter(letter) => palette_color(&char::from(letter).to_string(), 1.0),
        ModificationCode::Chebi(code) => palette_color(&format!("CHEBI:{code}"), 1.0),
    }
}

pub(crate) fn reference_position_for_read_position(
    alignment_start: u64,
    cigar: &[CigarOp],
    target_read_position: usize,
) -> Option<u64> {
    let mut reference_position = alignment_start;
    let mut read_position = 0_usize;
    for operation in cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                if (read_position..read_position.saturating_add(length))
                    .contains(&target_read_position)
                {
                    return Some(
                        reference_position.saturating_add(
                            target_read_position.saturating_sub(read_position) as u64,
                        ),
                    );
                }
                read_position = read_position.saturating_add(length);
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                if (read_position..read_position.saturating_add(length))
                    .contains(&target_read_position)
                {
                    return Some(reference_position);
                }
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_match_segment(
    display_list: &mut DisplayList,
    query: &AlignmentQuery,
    index: usize,
    kind: CigarOpKind,
    start: u64,
    end: u64,
    viewport: Viewport,
    y: f32,
    height: f32,
    object_id: u32,
    color_mode: AlignmentColorMode,
    pair_length_bounds: TemplateLengthBounds,
) {
    let (x, width) = clipped_geometry(start, end, viewport);
    if width <= 0.0 {
        return;
    }
    let explicit_mismatch = kind == CigarOpKind::SequenceMismatch
        && !matches!(color_mode, AlignmentColorMode::Bisulfite(_));
    let color = if explicit_mismatch {
        Rgba::new(0.96, 0.58, 0.18, 0.95)
    } else {
        alignment_color(query, index, color_mode, pair_length_bounds, 0.88)
    };
    let primitive_object_id = if explicit_mismatch {
        read_detail_object_id(object_id)
    } else {
        object_id
    };
    add_read_body(
        display_list,
        x,
        y,
        width.max(1.0),
        height,
        color,
        primitive_object_id,
    );
}

pub(crate) fn add_reference_bases(
    display_list: &mut DisplayList,
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
    reverse_complement: bool,
) {
    for position in visible_positions(reference.interval, viewport) {
        let Some(mut base) = reference.base_at(position) else {
            continue;
        };
        if reverse_complement {
            base = complement(base);
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        if width <= 0.0 {
            continue;
        }
        display_list.rectangles.push(RectPrimitive {
            x,
            y: top,
            width: width.max(1.0),
            height: 20.0,
            color: with_alpha(base_color(base), 0.32),
            object_id: 0,
        });
        if width >= 7.0 {
            add_base_glyph(
                display_list,
                base,
                x,
                top + 3.0,
                width,
                14.0,
                reference_base_label_color(base),
            );
        }
    }
}
