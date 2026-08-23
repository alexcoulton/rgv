use super::super::*;

pub(crate) fn variant_inspector_fields(
    inspector: &VariantInspectorSnapshot,
    record: &rgv_hts::VariantRecord,
) -> Vec<(String, String)> {
    vec![
        ("Track".to_owned(), inspector.track_name.clone()),
        (
            "Locus".to_owned(),
            format!("{}:{}-{}", record.contig, record.start + 1, record.end),
        ),
        (
            "ID".to_owned(),
            record.id.clone().unwrap_or_else(|| "missing".to_owned()),
        ),
        (
            "Type".to_owned(),
            record
                .variant_types
                .iter()
                .map(format_variant_type)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        ("REF".to_owned(), bounded_variant_allele(&record.reference)),
        (
            "ALT".to_owned(),
            record
                .alternates
                .iter()
                .map(|allele| bounded_variant_allele(allele))
                .collect::<Vec<_>>()
                .join(","),
        ),
        (
            "QUAL".to_owned(),
            record
                .quality
                .map_or_else(|| "missing".to_owned(), |quality| format!("{quality:.2}")),
        ),
        (
            "FILTER".to_owned(),
            if record.filters.is_empty() {
                "PASS".to_owned()
            } else {
                record.filters.join(";")
            },
        ),
    ]
}

pub(crate) fn bounded_variant_allele(allele: &[u8]) -> String {
    let end = allele.len().min(VARIANT_ALLELE_PREVIEW_BASES);
    let mut value = String::from_utf8_lossy(&allele[..end]).into_owned();
    if allele.len() > end {
        let _ = write!(value, "… (+{} bases)", allele.len() - end);
    }
    value
}

pub(crate) fn variant_allele_evidence_summary(
    record: &rgv_hts::VariantRecord,
    bin_length: u64,
    bin: rgv_hts::PileupBin,
) -> String {
    let first_type = record.variant_types.first();
    if bin_length == 1
        && record.reference.len() == 1
        && let Some(alternate) = record.alternates.first()
        && alternate.len() == 1
    {
        let base = alternate[0];
        return format!(
            "ALT {}: {} of {} base observations",
            char::from(base),
            bin.alleles.for_base(base),
            bin.alleles.base_observations(),
        );
    }
    if matches!(
        first_type,
        Some(rgv_hts::VariantType::Insertion | rgv_hts::VariantType::StructuralInsertion)
    ) {
        return format!("{} insertion-anchor observations", bin.alleles.insertion);
    }
    if matches!(
        first_type,
        Some(rgv_hts::VariantType::Deletion | rgv_hts::VariantType::StructuralDeletion)
    ) {
        return format!("{} deletion observations", bin.alleles.deletion);
    }
    format!(
        "A {} · C {} · G {} · T {} · N {} · insertions {} · deletions {}",
        bin.alleles.a,
        bin.alleles.c,
        bin.alleles.g,
        bin.alleles.t,
        bin.alleles.n,
        bin.alleles.insertion,
        bin.alleles.deletion,
    )
}

pub(crate) fn format_variant_type(variant_type: &rgv_hts::VariantType) -> String {
    match variant_type {
        rgv_hts::VariantType::Snv => "SNV".to_owned(),
        rgv_hts::VariantType::Mnv => "MNV".to_owned(),
        rgv_hts::VariantType::Insertion => "Insertion".to_owned(),
        rgv_hts::VariantType::Deletion => "Deletion".to_owned(),
        rgv_hts::VariantType::Complex => "Complex".to_owned(),
        rgv_hts::VariantType::StructuralDeletion => "Structural deletion".to_owned(),
        rgv_hts::VariantType::StructuralInsertion => "Structural insertion".to_owned(),
        rgv_hts::VariantType::Inversion => "Inversion".to_owned(),
        rgv_hts::VariantType::Duplication => "Duplication".to_owned(),
        rgv_hts::VariantType::CopyNumber => "Copy-number variant".to_owned(),
        rgv_hts::VariantType::Breakend => "Breakend".to_owned(),
        rgv_hts::VariantType::SpanningDeletion => "Spanning deletion".to_owned(),
        rgv_hts::VariantType::Symbolic(symbol) => format!("Symbolic <{symbol}>"),
    }
}

pub(crate) fn format_variant_field(field: &rgv_hts::VariantField) -> String {
    let mut value = match &field.value {
        rgv_hts::VariantValue::Flag(value) => value.to_string(),
        rgv_hts::VariantValue::Integers(values) => values
            .iter()
            .map(|value| value.map_or_else(|| ".".to_owned(), |value| value.to_string()))
            .collect::<Vec<_>>()
            .join(","),
        rgv_hts::VariantValue::Floats(values) => values
            .iter()
            .map(|value| value.map_or_else(|| ".".to_owned(), |value| value.to_string()))
            .collect::<Vec<_>>()
            .join(","),
        rgv_hts::VariantValue::Strings(values) => values.join(","),
    };
    if field.truncated {
        value.push_str(",…");
    }
    value
}

pub(crate) fn format_genotype(genotype: &rgv_hts::VariantGenotype) -> String {
    if genotype.alleles.is_empty() {
        return "GT=.".to_owned();
    }
    let mut value = String::from("GT=");
    for (index, allele) in genotype.alleles.iter().enumerate() {
        if index > 0 {
            value.push(if allele.phased { '|' } else { '/' });
        }
        match allele.index {
            None => value.push('.'),
            Some(allele_index) => {
                let _ = write!(value, "{allele_index}");
            }
        }
    }
    value
}

pub(crate) fn variant_genotype_group_rank(genotype: &rgv_hts::VariantGenotype) -> u8 {
    let indices = genotype
        .alleles
        .iter()
        .map(|allele| allele.index)
        .collect::<Option<Vec<_>>>();
    let Some(indices) = indices.filter(|indices| !indices.is_empty()) else {
        return 4;
    };
    let first = indices[0];
    if indices.iter().all(|index| *index == 0) {
        0
    } else if indices.iter().any(|index| *index != first) {
        1
    } else if first > 0 {
        2
    } else {
        3
    }
}

pub(crate) fn variant_genotype_group_label(genotype: &rgv_hts::VariantGenotype) -> &'static str {
    match variant_genotype_group_rank(genotype) {
        0 => "Hom-ref",
        1 => "Het",
        2 => "Hom-alt",
        3 => "Other called",
        _ => "Missing",
    }
}

pub(crate) fn variant_genotype_color(genotype: &rgv_hts::VariantGenotype) -> egui::Color32 {
    match variant_genotype_group_rank(genotype) {
        0 => egui::Color32::from_rgb(56, 122, 209),
        1 => egui::Color32::from_rgb(239, 137, 31),
        2 => egui::Color32::from_rgb(212, 51, 56),
        3 => egui::Color32::from_rgb(158, 76, 199),
        _ => egui::Color32::from_rgb(122, 128, 138),
    }
}

pub(crate) fn variant_details_text(
    fields: &[(String, String)],
    record: &rgv_hts::VariantRecord,
) -> String {
    let mut details = inspector_fields_text(fields);
    for field in &record.info {
        let _ = write!(
            details,
            "\nINFO/{}: {}",
            field.key,
            format_variant_field(field)
        );
    }
    if details.len() > VARIANT_DETAILS_COPY_BYTES {
        truncate_string_to_bytes(&mut details, VARIANT_DETAILS_COPY_BYTES - 32);
        details.push_str("\n… clipboard limit reached");
    }
    details
}

pub(crate) fn variant_export_file_name(record: &rgv_hts::VariantRecord, suffix: &str) -> String {
    let mut contig = record
        .contig
        .chars()
        .take(80)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if contig.is_empty() {
        contig.push_str("contig");
    }
    format!("rgv-{contig}-{}-{suffix}.tsv", record.start + 1)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn variant_genotype_export_tsv(inspector: &VariantInspectorSnapshot) -> String {
    let record = &inspector.query.records[inspector.record_index];
    let mut output = String::new();
    let mut complete = true;
    let filter_label = if record.filters.is_empty() {
        "PASS".to_owned()
    } else {
        record.filters.join(";")
    };
    for line in [
        "# RGV selected variant and genotype export v1".to_owned(),
        format!("# track\t{}", tsv_cell(&inspector.track_name)),
        format!(
            "# query_provenance\trecords_examined={}\trecords_filtered={}\trecord_limit_reached={}\tselected_samples={}\ttotal_samples={}\theader_samples_omitted={}\tgenotype_cells_omitted={}",
            inspector.query.provenance.records_examined,
            inspector.query.provenance.records_filtered,
            inspector.query.provenance.records_truncated,
            inspector.query.provenance.selected_samples,
            inspector.query.provenance.total_samples,
            inspector.query.provenance.header_samples_omitted,
            inspector.query.provenance.genotype_cells_omitted,
        ),
        format!(
            "# export_limits\tbytes={MAX_VARIANT_EXPORT_BYTES}\tfield_bytes={MAX_VARIANT_EXPORT_FIELD_BYTES}\tallele_preview_bytes={VARIANT_ALLELE_PREVIEW_BASES}",
        ),
        "section\tcontig\tstart_1based\tend_1based_inclusive\tid\tref\talt\tqual\tfilter\ttypes"
            .to_owned(),
        format!(
            "record\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            tsv_cell(&record.contig),
            record.start + 1,
            record.end,
            tsv_cell(record.id.as_deref().unwrap_or(".")),
            tsv_cell(&bounded_variant_allele(&record.reference)),
            tsv_cell(
                &record
                    .alternates
                    .iter()
                    .map(|allele| bounded_variant_allele(allele))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            record
                .quality
                .map_or_else(|| ".".to_owned(), |quality| quality.to_string()),
            tsv_cell(&filter_label),
            tsv_cell(
                &record
                    .variant_types
                    .iter()
                    .map(format_variant_type)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        "section\tkey\tvalue\tfield_truncated".to_owned(),
    ] {
        complete &= push_variant_export_line(&mut output, &line);
        if !complete {
            return output;
        }
    }
    for field in &record.info {
        let value = bounded_variant_export_field(field);
        let line = format!(
            "info\t{}\t{}\t{}",
            tsv_cell(&field.key),
            tsv_cell(&value),
            field.truncated || value.ends_with('…'),
        );
        if !push_variant_export_line(&mut output, &line) {
            return output;
        }
    }
    if !push_variant_export_line(
        &mut output,
        "section\tsample\tgenotype\tgroup\tformat_fields",
    ) {
        return output;
    }
    for genotype in &record.genotypes {
        let sample = inspector
            .query
            .header
            .samples
            .get(genotype.sample_index)
            .map_or("?", String::as_str);
        let mut format_fields = String::new();
        for field in &genotype.fields {
            if !format_fields.is_empty() {
                format_fields.push(';');
            }
            let value = bounded_variant_export_field(field);
            let _ = write!(format_fields, "{}={value}", field.key);
        }
        let line = format!(
            "genotype\t{}\t{}\t{}\t{}",
            tsv_cell(sample),
            tsv_cell(&format_genotype(genotype)),
            variant_genotype_group_label(genotype),
            tsv_cell(&format_fields),
        );
        if !push_variant_export_line(&mut output, &line) {
            return output;
        }
    }
    output
}

pub(crate) fn variant_evidence_export_tsv(
    inspector: &VariantInspectorSnapshot,
    evidence: &[VariantAlignmentEvidence],
) -> String {
    let record = &inspector.query.records[inspector.record_index];
    let mut output = String::new();
    for line in [
        "# RGV selected alignment evidence export v1".to_owned(),
        "# Observations are not an RGV variant call; overlap and co-location are not automatically labelled as support.".to_owned(),
        format!(
            "# variant\t{}:{}-{}\t{}",
            tsv_cell(&record.contig),
            record.start + 1,
            record.end,
            tsv_cell(record.id.as_deref().unwrap_or(".")),
        ),
        "alignment_track\tbin_start_1based\tbin_end_1based_inclusive\tdepth\tforward_depth\treverse_depth\tallele_observations\tmodification_calls\tmodification_series\tproper_pair_model_samples\tretained_records\tretained_records_sampled\tpileup_complete".to_owned(),
    ] {
        if !push_variant_export_line(&mut output, &line) {
            return output;
        }
    }
    for item in evidence {
        let line = format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            tsv_cell(&item.track_name),
            item.bin_start + 1,
            item.bin_end,
            item.depth,
            item.forward_depth,
            item.reverse_depth,
            tsv_cell(&item.allele_summary),
            item.modification_calls,
            item.modification_series,
            item.pair_templates_sampled,
            item.retained_records,
            item.retained_records_sampled,
            item.complete_pileup,
        );
        if !push_variant_export_line(&mut output, &line) {
            break;
        }
    }
    output
}

pub(crate) fn bounded_variant_export_field(field: &rgv_hts::VariantField) -> String {
    let mut value = format_variant_field(field);
    if value.len() > MAX_VARIANT_EXPORT_FIELD_BYTES {
        truncate_string_to_bytes(&mut value, MAX_VARIANT_EXPORT_FIELD_BYTES - '…'.len_utf8());
        value.push('…');
    }
    value
}

pub(crate) fn push_variant_export_line(output: &mut String, line: &str) -> bool {
    let required = line.len().saturating_add(1);
    if output.len().saturating_add(required) <= MAX_VARIANT_EXPORT_BYTES {
        output.push_str(line);
        output.push('\n');
        return true;
    }
    let marker = "# export_truncated\tbyte_limit_reached\n";
    if output.len().saturating_add(marker.len()) <= MAX_VARIANT_EXPORT_BYTES {
        output.push_str(marker);
    }
    false
}

pub(crate) fn tsv_cell(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            _ => character,
        })
        .collect()
}
