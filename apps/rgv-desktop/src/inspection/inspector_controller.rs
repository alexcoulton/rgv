use super::super::*;

pub(crate) fn focused_junction_component_value(
    component: JunctionArcComponent,
    junction: &rgv_hts::JunctionEvidence,
) -> String {
    let (label, depth) = match component {
        JunctionArcComponent::Positive => ("positive strand", junction.positive_depth),
        JunctionArcComponent::Negative => ("negative strand", junction.negative_depth),
        JunctionArcComponent::Unknown => ("unknown strand", junction.unknown_depth),
    };
    format!("{label} · {depth} supporting reads")
}

fn read_event_for_read_index(
    record: &AlignmentRecord,
    read_index: u32,
) -> Option<(usize, Option<u32>)> {
    let mut read_cursor = 0_u32;
    for (operation_index, operation) in record.cigar.iter().enumerate() {
        let consumes_read = matches!(
            operation.kind,
            CigarOpKind::Match
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
                | CigarOpKind::Insertion
                | CigarOpKind::SoftClip
        );
        if !consumes_read {
            continue;
        }
        let end = read_cursor.saturating_add(operation.len);
        if read_index >= read_cursor && read_index < end {
            let offset = matches!(
                operation.kind,
                CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch
            )
            .then_some(read_index - read_cursor);
            return Some((operation_index, offset));
        }
        read_cursor = end;
    }
    None
}

fn alignment_component_event(
    component: AlignmentComponent,
    record: &AlignmentRecord,
) -> Option<(usize, Option<u32>)> {
    let cigar_event = |cigar_index: u32| {
        let index = usize::try_from(cigar_index).ok()?;
        record.cigar.get(index).map(|_| (index, None))
    };
    match component {
        AlignmentComponent::Insertion { cigar_index, .. }
        | AlignmentComponent::Deletion { cigar_index }
        | AlignmentComponent::ReferenceSkip { cigar_index }
        | AlignmentComponent::SoftClip { cigar_index, .. }
        | AlignmentComponent::HardClip { cigar_index, .. }
        | AlignmentComponent::Padding { cigar_index, .. }
        | AlignmentComponent::UltimaIndel { cigar_index, .. }
        | AlignmentComponent::SbxInsertion { cigar_index, .. } => cigar_event(cigar_index),
        AlignmentComponent::Base {
            cigar_index,
            read_index,
            ..
        } => cigar_event(cigar_index).map(|(index, _)| {
            read_event_for_read_index(record, read_index).unwrap_or((index, None))
        }),
        AlignmentComponent::ModifiedBase { read_index, .. }
        | AlignmentComponent::BisulfiteBase { read_index, .. }
        | AlignmentComponent::SmrtKinetics { read_index, .. }
        | AlignmentComponent::SbxTailBase { read_index } => {
            read_event_for_read_index(record, read_index)
        }
        AlignmentComponent::ReadSpan
        | AlignmentComponent::AlignedSegment { .. }
        | AlignmentComponent::DirectionTerminal
        | AlignmentComponent::PairConnector { .. }
        | AlignmentComponent::Label { .. }
        | AlignmentComponent::SpecialistUnavailable { .. } => None,
    }
}

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn inspector_snapshot(&self) -> Option<InspectorSnapshot> {
        let panel = self.panels.active();
        let scene_hit = panel.selected_alignment_hit;
        if scene_hit.is_none() && !panel.navigation_presentation.is_interactive() {
            return None;
        }
        if scene_hit.is_none()
            && let Some(retained) = &panel.retained_evidence
            && matches!(
                retained.selection.kind,
                AutomationSelectionKind::Read | AutomationSelectionKind::ReadEvent
            )
        {
            return retained.inspector.clone();
        }
        let track_id = scene_hit.map_or_else(
            || {
                panel
                    .selected_alignment_track
                    .unwrap_or_else(|| self.primary_alignment_track_id())
            },
            |hit| hit.track,
        );
        let legacy_index = if scene_hit.is_none() {
            Some(panel.selected_record?)
        } else {
            None
        };
        let query = legacy_index.and_then(|_| self.complete_read_query_for_track(track_id));
        let scene_snapshot = scene_hit
            .and_then(|_| self.alignment_scene_snapshot_for_panel_track(panel.id, track_id));
        let record = if let Some(hit) = scene_hit {
            scene_snapshot?
                .inspect_retained(hit.handle)
                .ok()
                .flatten()?
        } else {
            query?.materialize_record(legacy_index?).ok()?
        };
        let reference = query.and_then(|query| query.reference.as_ref());
        let mate = match (record.mate_contig, record.mate_start) {
            (Some(contig_id), Some(position)) => {
                let name = self
                    .alignment_dictionary_for_track(track_id)
                    .and_then(|dictionary| dictionary.get(contig_id))
                    .map_or_else(
                        || format!("tid {}", contig_id.get()),
                        |contig| contig.name().to_owned(),
                    );
                format!("{name}:{}", position + 1)
            }
            _ => "missing".to_owned(),
        };
        let (sequence, read_sequence) = match &record.sequence {
            SequenceData::NotLoaded => ("not loaded".to_owned(), None),
            SequenceData::Missing => ("missing".to_owned(), None),
            SequenceData::Present { bases, qualities } => (
                format!(
                    "{} bases; qualities {}",
                    bases.len(),
                    qualities.as_ref().map_or("missing", |_| "present")
                ),
                Some(bounded_aligned_sequence_copy(bases)),
            ),
        };
        let modified_bases = decode_modified_bases(&record).map_or_else(
            |error| format!("error: {error}"),
            |calls| calls.len().to_string(),
        );
        let selected_filters = self.alignment_filters_for_track(track_id);
        let selected_event = scene_hit
            .and_then(|hit| alignment_component_event(hit.component, &record))
            .or_else(|| {
                let index = legacy_index?;
                panel
                    .selected_read_event
                    .filter(|event| event.record_index == index)
                    .map(|event| (event.operation_index, event.operation_offset))
            });
        let selected_operation_index = selected_event.map(|(operation_index, _)| operation_index);
        let pair_bounds = query.map_or_else(
            || {
                scene_snapshot.map_or(
                    TemplateLengthBounds {
                        minimum: selected_filters.minimum_template_length,
                        maximum: selected_filters.maximum_template_length,
                    },
                    |snapshot| {
                        snapshot
                            .reads()
                            .atlas()
                            .layout_contract()
                            .pair_length_bounds
                    },
                )
            },
            |query| effective_template_length_bounds_for_filters(&selected_filters, query),
        );
        let pair = pair_evidence(
            &record,
            self.panels.active().viewport.interval(),
            pair_bounds,
        );
        let pair_bounds_label = match (pair_bounds.minimum, pair_bounds.maximum) {
            (Some(minimum), Some(maximum)) => format!(
                "{minimum}–{maximum} bp ({})",
                if selected_filters.minimum_template_length.is_some()
                    || selected_filters.maximum_template_length.is_some()
                {
                    "explicit"
                } else {
                    "inferred"
                }
            ),
            _ => "unavailable".to_owned(),
        };
        let track_name = self.alignment_track_name_for_id(track_id);
        let cigar = format_cigar(&record.cigar);
        let segment_role = if record.flags.is_supplementary() {
            "Supplementary"
        } else if record.flags.is_secondary() {
            "Secondary"
        } else {
            "Primary"
        };
        let mut fields = vec![
            ("Track".to_owned(), track_name),
            ("Alignment segment".to_owned(), segment_role.to_owned()),
            (
                "Read".to_owned(),
                String::from_utf8_lossy(&record.read_name).into_owned(),
            ),
            (
                "Position".to_owned(),
                format!(
                    "{}:{}-{}",
                    self.panels.active().contig_name,
                    record.start + 1,
                    record.end
                ),
            ),
            ("Flags".to_owned(), format!("0x{:04x}", record.flags.bits())),
            (
                "MAPQ".to_owned(),
                record
                    .mapping_quality
                    .map_or_else(|| "missing".to_owned(), |value| value.to_string()),
            ),
            (
                "CIGAR".to_owned(),
                inspector_cigar_value(&record.cigar, &cigar),
            ),
            ("Mate".to_owned(), mate),
            (
                "Template length".to_owned(),
                record.template_length.to_string(),
            ),
            (
                "Template identity".to_owned(),
                format!("{:016x}", pair.template_id.get()),
            ),
            ("Expected template length".to_owned(), pair_bounds_label),
            (
                "Pair state".to_owned(),
                format!("{:?}", pair.classification),
            ),
            (
                "Pair orientation".to_owned(),
                format!("{:?}", pair.orientation),
            ),
            ("Read order".to_owned(), format!("{:?}", pair.read_order)),
            (
                "Mate visibility".to_owned(),
                format!("{:?}", pair.mate_visibility),
            ),
            ("Sequence".to_owned(), sequence),
            (
                "Sequence copy orientation".to_owned(),
                "reference-facing SAM/BAM alignment order".to_owned(),
            ),
            ("Modified-base calls".to_owned(), modified_bases),
        ];
        fields.extend(alignment_edit_fields(&record));
        if let AlignmentColorMode::Bisulfite(context) = selected_filters.alignment_color_mode {
            fields.push((
                "Bisulfite mode".to_owned(),
                format!(
                    "{} · {} first-of-pair strand",
                    bisulfite_context_label(context),
                    if bisulfite_strand_is_reverse(record.flags) {
                        "reverse"
                    } else {
                        "forward"
                    }
                ),
            ));
            if let Some(reference) = reference {
                let summary = summarize_bisulfite_record(&record, reference, context);
                let called = summary.protected.saturating_add(summary.converted);
                fields.push((
                    "Bisulfite observations".to_owned(),
                    format!(
                        "{} protected · {} converted · {} other context bases · {} context observations / {} aligned bases examined",
                        summary.protected,
                        summary.converted,
                        summary.other_context_bases,
                        summary.context_observations(),
                        summary.aligned_bases_examined,
                    ),
                ));
                fields.push((
                    "Conversion fraction".to_owned(),
                    if called == 0 {
                        "unavailable — no protected/converted observation in the loaded reference slice"
                            .to_owned()
                    } else {
                        format!("{:.4}", summary.converted as f64 / called as f64)
                    },
                ));
            } else {
                fields.push((
                    "Bisulfite observations".to_owned(),
                    "unavailable — load a matching reference FASTA".to_owned(),
                ));
            }
        }
        if let AlignmentColorMode::SmrtKinetics(mode) = selected_filters.alignment_color_mode {
            fields.push((
                "PacBio SMRT mode".to_owned(),
                format!(
                    "{} · {} alignment",
                    smrt_kinetics_mode_label(mode),
                    if record.flags.is_reverse() {
                        "reverse"
                    } else {
                        "forward"
                    }
                ),
            ));
            match smrt_kinetics_series(&record, mode) {
                Ok(series) => {
                    let summary = series.summary();
                    let tag = String::from_utf8_lossy(&summary.tag);
                    fields.push(("PacBio kinetic tag".to_owned(), tag.into_owned()));
                    fields.push((
                        "PacBio kinetic values".to_owned(),
                        format!(
                            "{} values · min {} · max {} · mean {:.2}",
                            summary.values,
                            summary.minimum.unwrap_or_default(),
                            summary.maximum.unwrap_or_default(),
                            summary.mean.unwrap_or_default(),
                        ),
                    ));
                }
                Err(error) => fields.push((
                    "PacBio kinetic values".to_owned(),
                    format!("unavailable — {error}"),
                )),
            }
        }
        if let AlignmentColorMode::Ultima(options) = selected_filters.alignment_color_mode {
            let read_group =
                legacy_index.and_then(|index| query.and_then(|query| query.read_group(index)));
            let (maximum_homopolymer_length, maximum_source) =
                options.maximum_homopolymer_length_override.map_or_else(
                    || {
                        (
                            read_group.and_then(|group| group.maximum_homopolymer_length),
                            "read-group mc",
                        )
                    },
                    |maximum| (Some(maximum), "track override"),
                );
            fields.push((
                "Ultima mode".to_owned(),
                ultima_quality_mode_label(options.quality_mode).to_owned(),
            ));
            fields.push((
                "Ultima read group".to_owned(),
                read_group.map_or_else(
                    || "unavailable".to_owned(),
                    |group| {
                        format!(
                            "{} · platform {}",
                            String::from_utf8_lossy(&group.id),
                            group.platform.as_deref().unwrap_or("missing"),
                        )
                    },
                ),
            ));
            fields.push((
                "Maximum homopolymer length".to_owned(),
                maximum_homopolymer_length.map_or_else(
                    || format!("unavailable ({maximum_source})"),
                    |maximum| format!("{maximum} bases ({maximum_source})"),
                ),
            ));
            let tag_orientation = read_group
                .and_then(|group| group.ultima_tag_orientation)
                .unwrap_or(UltimaTagOrientation::Reference);
            fields.push((
                "Ultima tp/t0 orientation".to_owned(),
                match read_group.and_then(|group| group.ultima_tag_orientation) {
                    Some(UltimaTagOrientation::Reference) => "reference (read-group tp)".to_owned(),
                    Some(UltimaTagOrientation::Synthesis) => "synthesis (read-group tp)".to_owned(),
                    None => "reference (assumed; read-group tp missing)".to_owned(),
                },
            ));
            match analyze_ultima_record_with_tag_orientation(
                &record,
                reference,
                options.quality_mode,
                maximum_homopolymer_length,
                tag_orientation,
            ) {
                Ok(analysis) => {
                    fields.push((
                        "Ultima flow-indel evidence".to_owned(),
                        format!(
                            "{} insertions · {} deletions · {} quality marks · {} unavailable events",
                            analysis.summary.insertions,
                            analysis.summary.deletions,
                            analysis.summary.quality_events,
                            analysis.summary.unavailable_events,
                        ),
                    ));
                    if let Some(event) = selected_operation_index
                        .and_then(|operation_index| analysis.event(operation_index))
                    {
                        fields.push((
                            "Ultima selected event".to_owned(),
                            format!(
                                "{} · {} bp{}",
                                ultima_indel_kind_label(event.kind),
                                event.length,
                                event.homopolymer_base.map_or_else(String::new, |base| {
                                    format!(" · {} homopolymer", char::from(base))
                                }),
                            ),
                        ));
                        fields.push((
                            "Ultima event quality".to_owned(),
                            event.quality.map_or_else(
                                || {
                                    format!(
                                        "unavailable — {}",
                                        event.unavailable.map_or(
                                            "no supported evidence",
                                            ultima_event_unavailable_label,
                                        )
                                    )
                                },
                                |quality| {
                                    format!(
                                        "Q{quality:.2} · {}",
                                        event.quality_source.map_or(
                                            "source unavailable",
                                            ultima_quality_source_label,
                                        )
                                    )
                                },
                            ),
                        ));
                        fields.push((
                            "Ultima flow direction".to_owned(),
                            event.direction.map_or_else(
                                || "unavailable".to_owned(),
                                |direction| {
                                    format!(
                                        "{direction} ({})",
                                        if direction < 0 {
                                            "insertion"
                                        } else {
                                            "deletion"
                                        }
                                    )
                                },
                            ),
                        ));
                    }
                }
                Err(error) => fields.push((
                    "Ultima flow-indel evidence".to_owned(),
                    format!("unavailable — {error}; the ordinary read and indels remain visible"),
                )),
            }
        }
        if let AlignmentColorMode::Sbx(options) = selected_filters.alignment_color_mode {
            fields.push((
                "SBX mode".to_owned(),
                format!(
                    "simplex tails {} · discordant Q≤{} · tail Q≤{}",
                    sbx_simplex_tail_mode_label(options.simplex_tail_mode),
                    options.discordant_quality_maximum,
                    options.simplex_tail_quality_maximum,
                ),
            ));
            match summarize_sbx_record(
                &record,
                options.discordant_quality_maximum,
                options.simplex_tail_quality_maximum,
            ) {
                Ok(summary) => {
                    fields.push((
                        "SBX simplex tails".to_owned(),
                        format!(
                            "{} left · {} right · {} retained bases",
                            summary.left_tail_bases,
                            summary.right_tail_bases,
                            summary.retained_bases,
                        ),
                    ));
                    fields.push((
                        "SBX insertion evidence".to_owned(),
                        format!(
                            "{} concordant · {} partly discordant · {} fully discordant{}",
                            summary.concordant_insertions,
                            summary.partly_discordant_insertions,
                            summary.fully_discordant_insertions,
                            if summary.unavailable_insertions == 0 {
                                String::new()
                            } else {
                                format!(" · {} unavailable", summary.unavailable_insertions)
                            },
                        ),
                    ));
                }
                Err(error) => fields.push((
                    "SBX evidence".to_owned(),
                    format!("unavailable — {error}; the read remains visible"),
                )),
            }
        }
        let auxiliary = record
            .auxiliary_fields
            .iter()
            .map(|field| {
                (
                    String::from_utf8_lossy(&field.tag).into_owned(),
                    format_aux_value(&field.value),
                )
            })
            .collect();
        let event_sequence = selected_event.and_then(|(operation_index, operation_offset)| {
            let details = read_event_details(
                &record,
                operation_index,
                operation_offset,
                reference,
                &self.panels.active().contig_name,
            )?;
            fields.extend(details.fields);
            details.copy_sequence
        });
        Some(InspectorSnapshot {
            fields,
            auxiliary,
            event_sequence,
            cigar: Some(cigar),
            read_sequence,
        })
    }

    pub(crate) fn effective_template_length_bounds(
        &self,
        query: &AlignmentQuery,
    ) -> TemplateLengthBounds {
        let inferred = query.pairs.inferred_bounds.unwrap_or_default();
        TemplateLengthBounds {
            minimum: self
                .primary_alignment_filters()
                .minimum_template_length
                .or(inferred.minimum),
            maximum: self
                .primary_alignment_filters()
                .maximum_template_length
                .or(inferred.maximum),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn pileup_inspector_snapshot(&self) -> Option<InspectorSnapshot> {
        if !self
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        if let Some(retained) = &self.panels.active().retained_evidence
            && retained.selection.kind == AutomationSelectionKind::Pileup
        {
            return retained.inspector.clone();
        }
        let position = self.panels.active().selected_pileup_position?;
        let track_id = self
            .panels
            .active()
            .selected_alignment_track
            .unwrap_or_else(|| self.primary_alignment_track_id());
        let query = self.alignment_query_for_track(track_id)?;
        let (index, bin) = query.pileup.bin_at(query.interval, position)?;
        let bin_interval = query.pileup.bin_interval(query.interval, index)?;
        let reference_base = (bin_interval.len() == 1)
            .then(|| query.reference.as_ref()?.base_at(bin_interval.start()))
            .flatten();
        let base_observations = bin.alleles.base_observations();
        let alternate_fraction = reference_base.and_then(|reference_base| {
            (base_observations > 0).then(|| {
                let reference = bin.alleles.for_base(reference_base);
                f64::from(base_observations.saturating_sub(reference))
                    / f64::from(base_observations)
            })
        });
        let interval_label = if bin_interval.len() == 1 {
            format!(
                "{}:{}",
                self.panels.active().contig_name,
                bin_interval.start() + 1
            )
        } else {
            format!(
                "{}:{}-{}",
                self.panels.active().contig_name,
                bin_interval.start() + 1,
                bin_interval.end()
            )
        };
        let weighted = &bin.quality_weighted;
        let weighted_value = |value: u64| value as f64 / QUALITY_WEIGHT_SCALE as f64;
        let fields = vec![
            (
                "Track".to_owned(),
                self.alignment_track_name_for_id(track_id),
            ),
            ("Locus".to_owned(), interval_label),
            (
                "Reference".to_owned(),
                reference_base.map_or_else(
                    || "unavailable".to_owned(),
                    |base| char::from(base).to_string(),
                ),
            ),
            ("Depth".to_owned(), format!("{}×", bin.depth)),
            (
                "Strand depth".to_owned(),
                format!(
                    "forward {} · reverse {}",
                    bin.forward_depth, bin.reverse_depth
                ),
            ),
            (
                "Alleles".to_owned(),
                format!(
                    "A {} · C {} · G {} · T {} · N {}",
                    bin.alleles.a, bin.alleles.c, bin.alleles.g, bin.alleles.t, bin.alleles.n
                ),
            ),
            (
                "Forward alleles".to_owned(),
                format!(
                    "A {} · C {} · G {} · T {} · N {}",
                    bin.forward_alleles.a,
                    bin.forward_alleles.c,
                    bin.forward_alleles.g,
                    bin.forward_alleles.t,
                    bin.forward_alleles.n
                ),
            ),
            (
                "Reverse alleles".to_owned(),
                format!(
                    "A {} · C {} · G {} · T {} · N {}",
                    bin.reverse_alleles.a,
                    bin.reverse_alleles.c,
                    bin.reverse_alleles.g,
                    bin.reverse_alleles.t,
                    bin.reverse_alleles.n
                ),
            ),
            (
                "Indel support".to_owned(),
                format!(
                    "deletion {} · insertion {}",
                    bin.alleles.deletion, bin.alleles.insertion
                ),
            ),
            (
                "Alternate fraction".to_owned(),
                alternate_fraction.map_or_else(
                    || "unavailable".to_owned(),
                    |value| format!("{:.2}%", value * 100.0),
                ),
            ),
            (
                "Quality-weighted".to_owned(),
                format!(
                    "A {:.2} · C {:.2} · G {:.2} · T {:.2} · N {:.2}",
                    weighted_value(weighted.a),
                    weighted_value(weighted.c),
                    weighted_value(weighted.g),
                    weighted_value(weighted.t),
                    weighted_value(weighted.n)
                ),
            ),
            (
                "Mean base quality".to_owned(),
                bin.mean_base_quality()
                    .map_or_else(|| "unavailable".to_owned(), |value| format!("{value:.1}")),
            ),
            (
                "Mean mapping quality".to_owned(),
                bin.mean_mapping_quality()
                    .map_or_else(|| "unavailable".to_owned(), |value| format!("{value:.1}")),
            ),
            (
                "Base quality filter".to_owned(),
                format!(">= {}", query.pileup.minimum_base_quality),
            ),
            (
                "Evidence".to_owned(),
                format!(
                    "all {} eligible reads · {} reads rendered{}",
                    query.pileup.provenance.eligible_records,
                    query.pileup.provenance.rendered_records,
                    if query.pileup.provenance.all_eligible_records {
                        ""
                    } else {
                        " · incomplete"
                    }
                ),
            ),
            (
                "Allele detail".to_owned(),
                if query.pileup.provenance.allele_observations_available {
                    "exact sequence observations loaded".to_owned()
                } else {
                    "zoom to base detail to load sequence observations".to_owned()
                },
            ),
            (
                "Overlapping mates".to_owned(),
                match query.pileup.provenance.overlapping_mate_policy {
                    OverlappingMatePolicy::CountOnce => format!(
                        "count once (leftmost primary end) · {} overlapping bases removed{}",
                        query.pileup.provenance.collapsed_mate_bases,
                        if query.pileup.provenance.overlap_collapse_complete {
                            ""
                        } else {
                            " · bounded collapse incomplete"
                        }
                    ),
                    OverlappingMatePolicy::CountBoth => "count both ends".to_owned(),
                },
            ),
        ];
        Some(InspectorSnapshot {
            fields,
            auxiliary: Vec::new(),
            event_sequence: None,
            cigar: None,
            read_sequence: None,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn modification_inspector_snapshot(&self) -> Option<InspectorSnapshot> {
        if !self
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        if let Some(retained) = &self.panels.active().retained_evidence
            && retained.selection.kind == AutomationSelectionKind::Modification
        {
            return retained.inspector.clone();
        }
        let position = self.panels.active().selected_modification_position?;
        let track_id = self
            .panels
            .active()
            .selected_alignment_track
            .unwrap_or_else(|| self.primary_alignment_track_id());
        let query = self.alignment_query_for_track(track_id)?;
        let summary = &query.pileup.modifications;
        let index = summary.bin_index_at(query.interval, position)?;
        let interval = summary.bin_interval(query.interval, index)?;
        let locus = if interval.len() == 1 {
            format!(
                "{}:{}",
                self.panels.active().contig_name,
                interval.start() + 1
            )
        } else {
            format!(
                "{}:{}-{}",
                self.panels.active().contig_name,
                interval.start() + 1,
                interval.end()
            )
        };
        let mut fields = vec![
            (
                "Track".to_owned(),
                self.alignment_track_name_for_id(track_id),
            ),
            ("Locus".to_owned(), locus),
        ];
        for series in &summary.series {
            let Some(bin) = series.bins.get(index).copied() else {
                continue;
            };
            if bin.modified_calls == 0 && bin.canonical_observations == 0 {
                continue;
            }
            let strand = match series.key.modification_strand {
                rgv_hts::BaseModificationStrand::Forward => '+',
                rgv_hts::BaseModificationStrand::Reverse => '-',
            };
            let mut label = format!(
                "{}{}{}",
                char::from(series.key.canonical_base),
                strand,
                modification_code_label(series.key.code)
            );
            if let Some(haplotype) = series.key.haplotype {
                let _ = write!(label, " · HP {haplotype}");
            }
            let raw = bin.raw_fraction().map_or_else(
                || "unknown denominator".to_owned(),
                |value| format!("{:.1}%", value * 100.0),
            );
            let weighted = bin.weighted_fraction().map_or_else(
                || "unavailable".to_owned(),
                |value| format!("{:.1}%", value * 100.0),
            );
            fields.push((
                label,
                format!(
                    "{} modified / {} canonical · raw {raw} · weighted {weighted} · forward {} / reverse {} · ML missing {}",
                    bin.modified_calls,
                    bin.canonical_observations,
                    bin.forward_modified_calls,
                    bin.reverse_modified_calls,
                    bin.missing_probability_calls
                ),
            ));
        }
        let provenance = summary.provenance;
        fields.push((
            "Evidence".to_owned(),
            format!(
                "{} tagged / {} eligible reads · {} calls · malformed {} · unavailable {}{}{}",
                provenance.records_with_modification_tags,
                provenance.eligible_records,
                provenance.decoded_calls,
                provenance.malformed_records,
                provenance.unavailable_records,
                if provenance.dropped_series > 0 {
                    format!(" · {} series dropped", provenance.dropped_series)
                } else {
                    String::new()
                },
                if provenance.all_eligible_records {
                    ""
                } else {
                    " · incomplete"
                }
            ),
        ));
        Some(InspectorSnapshot {
            fields,
            auxiliary: Vec::new(),
            event_sequence: None,
            cigar: None,
            read_sequence: None,
        })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn junction_inspector_snapshot(&self) -> Option<InspectorSnapshot> {
        if !self
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        let hovered = match self.panels.active().hover_inspector_target {
            Some(HoverInspectorTarget::Junction(track_id, start, end, component)) => {
                Some((track_id, start, end, component))
            }
            _ => None,
        };
        if hovered.is_none()
            && let Some(retained) = &self.panels.active().retained_evidence
            && retained.selection.kind == AutomationSelectionKind::Junction
        {
            return retained.inspector.clone();
        }
        let (track_id, start, end, focused_component) = hovered.map_or_else(
            || {
                let (start, end) = self.panels.active().selected_junction?;
                Some((
                    self.panels
                        .active()
                        .selected_alignment_track
                        .unwrap_or_else(|| self.primary_alignment_track_id()),
                    start,
                    end,
                    self.panels.active().selected_junction_component,
                ))
            },
            |(track_id, start, end, component)| Some((track_id, start, end, Some(component))),
        )?;
        let query = self.alignment_query_for_track(track_id)?;
        let junction = query
            .junctions
            .junctions
            .iter()
            .find(|junction| junction.start == start && junction.end == end)?;
        let provenance = query.junctions.provenance;
        let filters = self.alignment_filters_for_track(track_id);
        let sashimi_value =
            junction_count_value(&query.junctions, junction, filters.sashimi_count_mode);
        let sashimi_count = match (
            filters.sashimi_count_mode,
            provenance.normalization_mapped_records,
        ) {
            (SashimiCountMode::Raw, _) => format!("{} raw reads", junction.depth),
            (SashimiCountMode::CountsPerMillion, Some(total)) if total > 0 => {
                format!("{sashimi_value:.3} CPM using {total} whole-file mapped reads")
            }
            (SashimiCountMode::CountsPerMillion, _) => {
                format!(
                    "CPM unavailable from index metadata; showing {} raw reads",
                    junction.depth
                )
            }
        };
        let focused_component = focused_component
            .map(|component| focused_junction_component_value(component, junction));
        let annotation_context = junction_annotation_context(
            &self.tracks,
            &self.panels.active().track_snapshots,
            self.panels.active().track_snapshot_interval,
            self.panels.active().viewport.interval(),
        );
        let annotation_status = annotation_context.status(start, end);
        let annotation = match annotation_status {
            JunctionAnnotationStatus::Annotated => "annotated exon junction".to_owned(),
            JunctionAnnotationStatus::Novel => {
                "novel relative to the loaded visible annotation tracks".to_owned()
            }
            JunctionAnnotationStatus::Unknown if annotation_context.lookup.source_tracks == 0 => {
                "unknown — no visible gene annotation is loaded".to_owned()
            }
            JunctionAnnotationStatus::Unknown => {
                "unknown — the annotation query is incomplete or bounded".to_owned()
            }
        };
        let annotation_labels = annotation_context.labels(start, end);
        let mut fields = vec![
            (
                "Track".to_owned(),
                self.alignment_track_name_for_id(track_id),
            ),
            (
                "Skipped interval".to_owned(),
                format!("{}:{}-{}", self.panels.active().contig_name, start + 1, end),
            ),
            ("Intron length".to_owned(), format!("{} bp", end - start)),
            ("Depth".to_owned(), junction.depth.to_string()),
            ("Sashimi count".to_owned(), sashimi_count),
            ("Annotation status".to_owned(), annotation),
            (
                "Annotation evidence".to_owned(),
                if annotation_labels.is_empty() {
                    format!(
                        "{} visible annotation tracks · complete interval {} · record cap reached {}",
                        annotation_context.lookup.source_tracks,
                        annotation_context.lookup.complete_for_interval,
                        annotation_context.source_records_truncated
                            || annotation_context.lookup.truncated,
                    )
                } else {
                    annotation_labels.join(", ")
                },
            ),
            (
                "Aligned flanks".to_owned(),
                format!(
                    "left {}–{} bp · right {}–{} bp · query minimum {} bp each side",
                    junction.minimum_left_flank,
                    junction.maximum_left_flank,
                    junction.minimum_right_flank,
                    junction.maximum_right_flank,
                    provenance.minimum_flank
                ),
            ),
            (
                "Strand support".to_owned(),
                format!(
                    "+ {} · − {} · unknown {}",
                    junction.positive_depth, junction.negative_depth, junction.unknown_depth
                ),
            ),
            (
                "Evidence".to_owned(),
                format!(
                    "{} supporting / {} spliced / {} eligible reads · {} observations retained / {} examined · {} removed by flank filter{}",
                    provenance.supporting_spliced_records,
                    provenance.spliced_records,
                    provenance.eligible_records,
                    provenance.junction_observations_retained,
                    provenance.junction_observations_examined,
                    provenance.junction_observations_filtered_by_flank,
                    if provenance.all_eligible_records {
                        ""
                    } else {
                        " · incomplete"
                    }
                ),
            ),
            (
                "Strand provenance".to_owned(),
                format!(
                    "selected junction: XS +{} / −{} / unknown {} · alignment fallback +{} / −{} · invalid XS {}. Query totals: XS {} · fallback {} · invalid XS {}",
                    junction.tagged_positive_depth,
                    junction.tagged_negative_depth,
                    junction.tagged_unknown_depth,
                    junction.fallback_positive_depth,
                    junction.fallback_negative_depth,
                    junction.invalid_strand_depth,
                    provenance.xs_tagged_observations,
                    provenance.alignment_strand_fallback_observations,
                    provenance.invalid_xs_observations,
                ),
            ),
            (
                "Bounded aggregation".to_owned(),
                if provenance.dropped_distinct_junctions == 0
                    && provenance.dropped_record_junction_observations == 0
                {
                    "complete".to_owned()
                } else {
                    format!(
                        "{} distinct junctions dropped at the {}-junction cap · {} excess splice operations dropped at the per-record cap",
                        provenance.dropped_distinct_junctions,
                        rgv_hts::MAX_JUNCTIONS,
                        provenance.dropped_record_junction_observations,
                    )
                },
            ),
        ];
        if let Some(focused_component) = focused_component {
            fields.insert(4, ("Focused component".to_owned(), focused_component));
        }
        Some(InspectorSnapshot {
            fields,
            auxiliary: Vec::new(),
            event_sequence: None,
            cigar: None,
            read_sequence: None,
        })
    }
}

#[cfg(test)]
mod scene_inspection_tests {
    use super::*;

    fn record() -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 100,
            end: 110,
            flags: rgv_hts::AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"scene-read".to_vec(),
            cigar: vec![
                rgv_hts::CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
                rgv_hts::CigarOp {
                    kind: CigarOpKind::Insertion,
                    len: 3,
                },
                rgv_hts::CigarOp {
                    kind: CigarOpKind::SequenceMismatch,
                    len: 5,
                },
            ],
            sequence: SequenceData::Present {
                bases: vec![b'A'; 13],
                qualities: Some(vec![30; 13]),
            },
            auxiliary_fields: Vec::new(),
        }
    }

    #[test]
    fn scene_components_resolve_inspector_events_without_query_indices() {
        let record = record();
        assert_eq!(
            alignment_component_event(
                AlignmentComponent::Base {
                    cigar_index: 2,
                    read_index: 10,
                    base: b'A',
                    quality: Some(30),
                    classification: AlignedBaseClassification::Substitution,
                },
                &record,
            ),
            Some((2, Some(2)))
        );
        assert_eq!(
            alignment_component_event(
                AlignmentComponent::Insertion {
                    cigar_index: 1,
                    read_start: 5,
                    length: 3,
                },
                &record,
            ),
            Some((1, None))
        );
    }
}
