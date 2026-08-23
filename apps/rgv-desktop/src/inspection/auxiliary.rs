use super::super::*;

#[allow(clippy::too_many_lines)]
pub(crate) fn feature_inspector_fields(
    inspector: &FeatureInspectorSnapshot,
) -> Vec<(String, String)> {
    let record = &inspector.record;
    let mut fields = vec![
        ("Track".to_owned(), inspector.track_name.clone()),
        (
            "Name".to_owned(),
            record.name.clone().unwrap_or_else(|| "unnamed".to_owned()),
        ),
        (
            "Type".to_owned(),
            record
                .feature_type
                .clone()
                .unwrap_or_else(|| "feature".to_owned()),
        ),
        (
            "Location".to_owned(),
            format!(
                "{}:{}-{} ({} bp; one-based display)",
                record.contig,
                record.start.saturating_add(1),
                record.end,
                record.end.saturating_sub(record.start)
            ),
        ),
        (
            "Strand".to_owned(),
            match record.strand {
                rgv_hts::Strand::Forward => "+",
                rgv_hts::Strand::Reverse => "-",
                rgv_hts::Strand::Unstranded => "unstranded",
            }
            .to_owned(),
        ),
        (
            "Score".to_owned(),
            record
                .score
                .map_or_else(|| "not supplied".to_owned(), |score| score.to_string()),
        ),
    ];
    if let Some((start, end)) = record.coding_interval {
        fields.push((
            "Coding sequence".to_owned(),
            format!("{}-{} (one-based display)", start.saturating_add(1), end),
        ));
    } else if record.feature_type.as_deref() == Some("transcript") {
        fields.push(("Coding sequence".to_owned(), "non-coding / none".to_owned()));
    }
    if !record.blocks.is_empty() {
        let mut exons = record
            .blocks
            .iter()
            .take(MAX_FEATURE_INSPECTOR_BLOCKS)
            .map(|(start, end)| format!("{}-{}", start.saturating_add(1), end))
            .collect::<Vec<_>>()
            .join(", ");
        if record.blocks.len() > MAX_FEATURE_INSPECTOR_BLOCKS {
            let _ = write!(
                exons,
                " … [{} more omitted]",
                record.blocks.len() - MAX_FEATURE_INSPECTOR_BLOCKS
            );
        }
        let block_label = if record.feature_type.as_deref() == Some("PSL alignment") {
            "Alignment blocks"
        } else {
            "Exons"
        };
        fields.push((format!("{block_label} ({})", record.blocks.len()), exons));
    }
    if let Some(peak) = record.peak {
        fields.extend([
            ("Signal value".to_owned(), peak.signal_value.to_string()),
            (
                "p-value (-log10)".to_owned(),
                peak.p_value
                    .map_or_else(|| "not supplied".to_owned(), |value| value.to_string()),
            ),
            (
                "q-value (-log10 FDR)".to_owned(),
                peak.q_value
                    .map_or_else(|| "not supplied".to_owned(), |value| value.to_string()),
            ),
            (
                "Called summit".to_owned(),
                peak.summit.map_or_else(
                    || "not supplied".to_owned(),
                    |summit| {
                        format!(
                            "{}:{} (offset {} bp)",
                            record.contig,
                            summit.saturating_add(1),
                            summit.saturating_sub(record.start)
                        )
                    },
                ),
            ),
        ]);
    }
    for (key, value) in record
        .attributes
        .iter()
        .take(MAX_FEATURE_INSPECTOR_ATTRIBUTES)
    {
        let mut value = value.clone();
        if value.len() > MAX_FEATURE_INSPECTOR_VALUE_BYTES {
            truncate_string_to_bytes(&mut value, MAX_FEATURE_INSPECTOR_VALUE_BYTES - 3);
            value.push('…');
        }
        fields.push((format!("Attribute · {key}"), value));
    }
    if record.attributes.len() > MAX_FEATURE_INSPECTOR_ATTRIBUTES {
        fields.push((
            "Attribute provenance".to_owned(),
            format!(
                "{} additional attributes omitted from this inspector",
                record.attributes.len() - MAX_FEATURE_INSPECTOR_ATTRIBUTES
            ),
        ));
    }
    fields
}

pub(crate) fn interaction_inspector_fields(
    inspector: &InteractionInspectorSnapshot,
) -> Vec<(String, String)> {
    let record = &inspector.query.records[inspector.record_index];
    let endpoint = |endpoint: &rgv_hts::InteractionEndpoint| {
        format!(
            "{}:{}-{}{}{}",
            endpoint.contig,
            endpoint.start.saturating_add(1),
            endpoint.end,
            endpoint
                .name
                .as_deref()
                .map_or_else(String::new, |name| format!(" · {name}")),
            match endpoint.strand {
                rgv_hts::Strand::Forward => " · +",
                rgv_hts::Strand::Reverse => " · -",
                rgv_hts::Strand::Unstranded => "",
            },
        )
    };
    let provenance = inspector.query.provenance;
    vec![
        ("Track".to_owned(), inspector.track_name.clone()),
        (
            "Name".to_owned(),
            record.name.clone().unwrap_or_else(|| "unnamed".to_owned()),
        ),
        ("Source".to_owned(), endpoint(&record.source)),
        ("Target".to_owned(), endpoint(&record.target)),
        (
            "Directionality".to_owned(),
            if record.directional {
                "source → target"
            } else {
                "undirected"
            }
            .to_owned(),
        ),
        (
            "Score / value".to_owned(),
            format!(
                "{} / {}",
                record
                    .score
                    .map_or_else(|| "missing".to_owned(), |value| value.to_string()),
                record
                    .value
                    .map_or_else(|| "missing".to_owned(), |value| value.to_string()),
            ),
        ),
        (
            "Experiment".to_owned(),
            record
                .experiment
                .clone()
                .unwrap_or_else(|| "not supplied".to_owned()),
        ),
        (
            "Display provenance".to_owned(),
            format!(
                "{} · minimum score {} · scale {} · labels {}",
                match inspector.settings.display_mode {
                    InteractionDisplayMode::Arcs => "arcs",
                    InteractionDisplayMode::Blocks => "blocks",
                },
                inspector
                    .settings
                    .minimum_score
                    .map_or_else(|| "none".to_owned(), |value| value.to_string()),
                inspector
                    .settings
                    .fixed_maximum_score
                    .map_or_else(|| "automatic".to_owned(), |value| format!("fixed {value}")),
                if inspector.settings.show_labels {
                    "shown"
                } else {
                    "hidden"
                },
            ),
        ),
        (
            "Query provenance".to_owned(),
            format!(
                "{} endpoint entries examined · {} duplicate entries ignored · {} filtered{}",
                provenance.endpoint_entries_examined,
                provenance.duplicate_entries_ignored,
                provenance.records_filtered,
                if provenance.records_truncated {
                    " · record cap reached"
                } else {
                    " · complete"
                },
            ),
        ),
    ]
}

pub(crate) fn endpoint_center_position(start: u64, end: u64) -> u64 {
    start.saturating_add(end.saturating_sub(start) / 2)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn quantitative_inspector_fields(
    inspector: &QuantitativeInspectorSnapshot,
) -> Vec<(String, String)> {
    let record = &inspector.query.records[inspector.record_index];
    let settings = &inspector.settings;
    let scale = match settings.scale_mode {
        QuantitativeScaleMode::Automatic => "per-track automatic".to_owned(),
        QuantitativeScaleMode::Fixed => format!(
            "fixed {} to {}",
            settings.fixed_minimum, settings.fixed_maximum
        ),
        QuantitativeScaleMode::Shared => "shared visible compatible tracks".to_owned(),
    };
    let transform = match settings.scale_transform {
        QuantitativeScaleTransform::Linear => "linear",
        QuantitativeScaleTransform::SignedLog10 => "signed log10(1 + |value|)",
    };
    let window = match settings.window_function {
        QuantitativeWindowFunction::Raw => "raw / no windowing",
        QuantitativeWindowFunction::Minimum => "minimum per screen window",
        QuantitativeWindowFunction::Mean => "base-weighted mean per screen window",
        QuantitativeWindowFunction::Maximum => "maximum per screen window",
    };
    let provenance = inspector.query.provenance;
    let mut fields = vec![
        ("Track".to_owned(), inspector.track_name.clone()),
        (
            "Selected position".to_owned(),
            inspector.selected_position.saturating_add(1).to_string(),
        ),
        (
            "Source interval".to_owned(),
            format!(
                "{}-{} ({} bp)",
                record.start.saturating_add(1),
                record.end,
                record.end.saturating_sub(record.start)
            ),
        ),
        (
            "Source value".to_owned(),
            if record.value.is_finite() {
                format!("{}", record.value)
            } else {
                "non-finite / treated as missing".to_owned()
            },
        ),
        ("Baseline".to_owned(), settings.baseline.to_string()),
        (
            "Displayed scale".to_owned(),
            format!("{scale} · {transform}"),
        ),
        (
            "Window provenance".to_owned(),
            format!(
                "{window}; selected value is the exact source interval, not its rendered aggregate"
            ),
        ),
        (
            "Query provenance".to_owned(),
            format!(
                "{} retained / {} examined · {} non-finite{}",
                provenance.records_retained,
                provenance.records_examined,
                provenance.non_finite_records,
                if provenance.records_truncated {
                    " · source record cap reached"
                } else {
                    " · complete"
                }
            ),
        ),
    ];
    if let Some(rgv_hts::QuantitativeSourceMetadata::Tdf(metadata)) =
        &inspector.query.source_metadata
    {
        fields.push((
            "TDF channel".to_owned(),
            metadata.selected_track_name.clone(),
        ));
        fields.push((
            "TDF channels".to_owned(),
            format!(
                "{} of {}{}",
                metadata.selected_track_index + 1,
                metadata.track_count,
                if metadata.track_count > 1 {
                    " · every embedded channel is opened as an independent track"
                } else {
                    ""
                }
            ),
        ));
        fields.push(("TDF dataset".to_owned(), metadata.dataset.clone()));
        fields.push((
            "TDF resolution".to_owned(),
            metadata.zoom.map_or_else(
                || "raw / unwindowed".to_owned(),
                |zoom| format!("zoom {zoom} · {}", metadata.window_function),
            ),
        ));
        fields.push((
            "TDF format".to_owned(),
            format!(
                "version {} · {}{}",
                metadata.version,
                metadata.data_type,
                if metadata.compressed {
                    " · compressed tiles"
                } else {
                    ""
                }
            ),
        ));
        if let Some(genome_id) = &metadata.genome_id {
            fields.push(("TDF genome".to_owned(), genome_id.clone()));
        }
    }
    if settings.arithmetic != QuantitativeArithmetic::None {
        let operation = match settings.arithmetic {
            QuantitativeArithmetic::None => unreachable!(),
            QuantitativeArithmetic::Add => "add",
            QuantitativeArithmetic::Subtract => "subtract",
            QuantitativeArithmetic::Multiply => "multiply",
            QuantitativeArithmetic::Divide => "divide",
        };
        let operand = inspector.operand.as_ref();
        fields.push((
            "Arithmetic".to_owned(),
            format!(
                "{operation} {}",
                operand.map_or("unavailable operand", |operand| operand.name.as_str())
            ),
        ));
        let derived = operand.and_then(|operand| {
            quantitative_arithmetic_value(record.value, operand.value?, settings.arithmetic)
        });
        fields.push((
            "Operand value".to_owned(),
            operand.and_then(|operand| operand.value).map_or_else(
                || "missing at selected position".to_owned(),
                |value| value.to_string(),
            ),
        ));
        fields.push((
            "Derived value".to_owned(),
            derived.map_or_else(
                || {
                    if settings.arithmetic == QuantitativeArithmetic::Divide
                        && operand.and_then(|operand| operand.value) == Some(0.0)
                    {
                        "missing (division by zero)".to_owned()
                    } else {
                        "missing because both finite operands are required".to_owned()
                    }
                },
                |value| value.to_string(),
            ),
        ));
    }
    fields
}

pub(crate) fn gwas_inspector_fields(inspector: &GwasInspectorSnapshot) -> Vec<(String, String)> {
    let record = &inspector.query.records[inspector.record_index];
    let provenance = inspector.query.provenance;
    let exact_p = record
        .fields
        .iter()
        .find(|(key, _)| {
            matches!(
                key.trim_start_matches('#').to_ascii_lowercase().as_str(),
                "p" | "pval" | "p-value" | "pvalue" | "p.value"
            )
        })
        .map_or_else(|| record.p_value.to_string(), |(_, value)| value.clone());
    let mut fields = vec![
        ("Track".to_owned(), inspector.track_name.clone()),
        ("Association".to_owned(), record.identifier.clone()),
        (
            "Location".to_owned(),
            format!(
                "{}:{} (one-based display)",
                record.contig,
                record.position + 1
            ),
        ),
        ("Exact source p-value".to_owned(), exact_p),
        (
            "-log10(p)".to_owned(),
            if record.minus_log10_p.is_infinite() {
                "+infinity (source p-value is zero; displayed at scale maximum)".to_owned()
            } else {
                record.minus_log10_p.to_string()
            },
        ),
        (
            "Display".to_owned(),
            format!(
                "{} px points · maximum -log10(p) {} · {} colours",
                inspector.settings.point_size,
                inspector.settings.maximum_score,
                match inspector.settings.color_mode {
                    GwasColorMode::Chromosome => "chromosome palette",
                    GwasColorMode::Alternating => "alternating chromosome",
                    GwasColorMode::Uniform => "uniform",
                }
            ),
        ),
        (
            "Query provenance".to_owned(),
            format!(
                "{} retained / {} examined · {} missing-p rows ignored{}",
                provenance.records_retained,
                provenance.records_examined,
                provenance.source_rows_ignored,
                if provenance.records_truncated {
                    " · positional strongest-point cap applied"
                } else {
                    " · complete"
                }
            ),
        ),
    ];
    fields.extend(
        record
            .fields
            .iter()
            .map(|(key, value)| (format!("Source column · {key}"), value.clone())),
    );
    fields
}

pub(crate) fn copy_number_inspector_fields(
    inspector: &CopyNumberInspectorSnapshot,
) -> Vec<(String, String)> {
    let segment = &inspector.query.segments[inspector.segment_index];
    let sample = inspector
        .query
        .samples
        .get(segment.sample_index)
        .map_or("unavailable", String::as_str);
    let semantics = effective_copy_number_semantics_for_settings(
        &inspector.settings,
        inspector.query.inferred_value_semantics,
    );
    let order = copy_number_sample_order(
        inspector.query.samples.len(),
        &inspector.query.sample_sort_values,
        inspector.settings.sort_mode,
    );
    let rank = order
        .iter()
        .position(|candidate| *candidate == segment.sample_index)
        .map(|rank| rank.saturating_add(1));
    let provenance = inspector.query.provenance;
    let mut fields = vec![
        ("Track".to_owned(), inspector.track_name.clone()),
        ("Sample".to_owned(), sample.to_owned()),
        (
            "Segment".to_owned(),
            format!(
                "{}-{} ({} bp; one-based display)",
                segment.start.saturating_add(1),
                segment.end,
                segment.end.saturating_sub(segment.start)
            ),
        ),
        ("Value".to_owned(), segment.value.to_string()),
        (
            "Markers".to_owned(),
            segment
                .marker_count
                .map_or_else(|| "not supplied".to_owned(), |count| count.to_string()),
        ),
        (
            "Value interpretation".to_owned(),
            match semantics {
                rgv_hts::CopyNumberValueSemantics::LogRatio => {
                    "log ratio; neutral baseline 0".to_owned()
                }
                rgv_hts::CopyNumberValueSemantics::Absolute => {
                    "absolute copy number; neutral baseline 2".to_owned()
                }
            },
        ),
        (
            "Row order".to_owned(),
            match inspector.settings.sort_mode {
                CopyNumberSortMode::FileOrder => format!("file order; row {}", rank.unwrap_or(0)),
                CopyNumberSortMode::Amplification => {
                    format!("amplification descending; row {}", rank.unwrap_or(0))
                }
                CopyNumberSortMode::Deletion => {
                    format!("deletion ascending; row {}", rank.unwrap_or(0))
                }
            },
        ),
        (
            "Query provenance".to_owned(),
            format!(
                "{} retained / {} examined · {} ignored source rows{}",
                provenance.records_retained,
                provenance.records_examined,
                provenance.source_rows_ignored,
                if provenance.records_truncated {
                    " · segment cap reached"
                } else {
                    " · complete"
                }
            ),
        ),
    ];
    if let Some(locus) = &inspector.settings.sort_locus
        && inspector.settings.sort_mode != CopyNumberSortMode::FileOrder
    {
        fields.push((
            "Sort locus".to_owned(),
            format!(
                "{}:{}-{} · base-weighted mean{}",
                locus.contig,
                locus.start.saturating_add(1),
                locus.end,
                if provenance.sort_records_truncated {
                    " · capped"
                } else {
                    ""
                }
            ),
        ));
    }
    fields
}

pub(crate) fn mutation_inspector_fields(
    inspector: &MutationInspectorSnapshot,
) -> Vec<(String, String)> {
    let record = &inspector.query.records[inspector.record_index];
    let sample = inspector
        .query
        .samples
        .get(record.sample_index)
        .map_or("unavailable", String::as_str);
    let provenance = inspector.query.provenance;
    let mut fields = vec![
        ("Track".to_owned(), inspector.track_name.clone()),
        ("Sample".to_owned(), sample.to_owned()),
        (
            "Mutation".to_owned(),
            if record.insertion_boundary {
                format!(
                    "{}: Start_Position {} / End_Position {} (insertion boundary)",
                    inspector.contig, record.source_start, record.source_end
                )
            } else {
                format!(
                    "{}:{}-{} (one-based display)",
                    inspector.contig, record.source_start, record.source_end
                )
            },
        ),
        ("Gene".to_owned(), record.gene.clone()),
        ("Classification".to_owned(), record.classification.clone()),
        ("Variant type".to_owned(), record.variant_type.clone()),
        ("Reference allele".to_owned(), record.reference.clone()),
        (
            "Tumour allele 1".to_owned(),
            record.tumor_allele_one.clone(),
        ),
        (
            "Tumour allele 2".to_owned(),
            record.tumor_allele_two.clone(),
        ),
    ];
    for annotation in &record.annotations {
        let label = inspector
            .query
            .headers
            .get(usize::from(annotation.column_index))
            .map_or_else(
                || format!("MAF column {}", usize::from(annotation.column_index) + 1),
                |header| format!("MAF · {header}"),
            );
        fields.push((
            label,
            if annotation.truncated {
                format!("{}… [value truncated]", annotation.value)
            } else {
                annotation.value.clone()
            },
        ));
    }
    fields.push((
        "Query provenance".to_owned(),
        format!(
            "{} retained / {} examined · {} ignored source rows{}",
            provenance.records_retained,
            provenance.records_examined,
            provenance.source_rows_ignored,
            if provenance.records_truncated {
                " · mutation cap reached"
            } else {
                " · complete"
            }
        ),
    ));
    fields.push((
        "Annotation provenance".to_owned(),
        format!(
            "{} omitted header columns · {} truncated/omitted values · {:.2} MiB retained source annotations",
            provenance.header_columns_omitted,
            provenance.annotation_values_truncated,
            provenance.source_annotation_bytes as f64 / (1_024.0 * 1_024.0)
        ),
    ));
    fields
}
