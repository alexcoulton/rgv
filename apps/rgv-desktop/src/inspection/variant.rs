use crate::inspection::document::{
    InspectorAction, InspectorActionId, InspectorBadge, InspectorDocument, InspectorField,
    InspectorLocation, InspectorNotice, InspectorProvenance, InspectorSection, InspectorTable,
    InspectorTone, InspectorValue,
};
use crate::inspection::model::{VariantAlignmentEvidence, VariantInspectorSnapshot};
use crate::inspection::read::{inspector_fields_text, truncate_string_to_bytes};
use crate::{
    MAX_VARIANT_EXPORT_BYTES, MAX_VARIANT_EXPORT_FIELD_BYTES, VARIANT_ALLELE_PREVIEW_BASES,
    VARIANT_DETAILS_COPY_BYTES,
};
use rgv_core::{SampleMetadataTable, VariantGenotypeGrouping};
use std::fmt::Write as _;
use std::sync::Arc;

pub(crate) fn variant_inspector_fields(
    inspector: &VariantInspectorSnapshot,
    record: &rgv_hts::VariantRecord,
) -> Vec<(String, String)> {
    variant_inspector_document(inspector, record).flatten_automation_fields()
}

pub(crate) fn variant_inspector_document(
    inspector: &VariantInspectorSnapshot,
    record: &rgv_hts::VariantRecord,
) -> InspectorDocument {
    variant_inspector_document_with_metadata(inspector, record, None, None)
}

#[allow(clippy::too_many_lines)]
fn variant_inspector_document_with_metadata(
    inspector: &VariantInspectorSnapshot,
    record: &rgv_hts::VariantRecord,
    sample_metadata: Option<&SampleMetadataTable>,
    sample_search: Option<&str>,
) -> InspectorDocument {
    const MAX_PRESENTED_GENOTYPE_ROWS: usize = 512;
    let title = record.id.clone().unwrap_or_else(|| {
        record
            .variant_types
            .first()
            .map_or_else(|| "Variant".to_owned(), format_variant_type)
    });
    let legacy_fields = variant_inspector_legacy_fields(inspector, record);
    let mut document = InspectorDocument::from_legacy_fields(
        format!(
            "variant:{}:{}:{}:{}",
            inspector.track_name, record.contig, record.start, record.end
        ),
        title,
        legacy_fields.clone(),
    );
    document.header.subtitle = Some(
        record
            .variant_types
            .iter()
            .map(format_variant_type)
            .collect::<Vec<_>>()
            .join(" · "),
    );
    document.header.source = Some(inspector.track_name.clone());
    document.header.location = Some(InspectorLocation {
        contig: record.contig.clone(),
        start: record.start,
        end: record.end,
        strand: None,
        display: format!("{}:{}–{}", record.contig, record.start + 1, record.end),
    });
    document.header.badges.push(InspectorBadge {
        label: if record.filters.is_empty() {
            "PASS".to_owned()
        } else {
            record.filters.join(";")
        },
        tone: if record.filters.is_empty() {
            InspectorTone::Positive
        } else {
            InspectorTone::Warning
        },
    });
    if let Some(summary) = document.sections.first_mut() {
        "variant-summary".clone_into(&mut summary.id);
        "Variant summary".clone_into(&mut summary.title);
        summary.fields.retain(|field| {
            !(field.label == "ID" && record.id.is_none()
                || field.label == "QUAL" && record.quality.is_none())
        });
        if let Some(reference) = summary.fields.iter_mut().find(|field| field.label == "REF") {
            let complete = String::from_utf8_lossy(&record.reference).into_owned();
            let display = bounded_variant_allele(&record.reference);
            reference.value = InspectorValue::Sequence {
                display,
                total_bases: record.reference.len(),
                truncated: record.reference.len() > VARIANT_ALLELE_PREVIEW_BASES,
            };
            reference.copy_value = Some(complete);
            if record.reference.len() > VARIANT_ALLELE_PREVIEW_BASES {
                reference.provenance = InspectorProvenance::Truncated {
                    displayed: VARIANT_ALLELE_PREVIEW_BASES,
                    total: Some(record.reference.len()),
                    reason: "allele preview limit".to_owned(),
                };
            }
        }
        if let Some(alternates) = summary.fields.iter_mut().find(|field| field.label == "ALT") {
            let complete = record
                .alternates
                .iter()
                .map(|allele| String::from_utf8_lossy(allele).into_owned())
                .collect::<Vec<_>>()
                .join(",");
            let display = record
                .alternates
                .iter()
                .map(|allele| bounded_variant_allele(allele))
                .collect::<Vec<_>>()
                .join(", ");
            let truncated = record
                .alternates
                .iter()
                .any(|allele| allele.len() > VARIANT_ALLELE_PREVIEW_BASES);
            alternates.value = InspectorValue::Identifier(display);
            alternates.copy_value = Some(complete);
            if truncated {
                alternates.provenance = InspectorProvenance::Truncated {
                    displayed: record
                        .alternates
                        .iter()
                        .map(|allele| allele.len().min(VARIANT_ALLELE_PREVIEW_BASES))
                        .sum(),
                    total: Some(record.alternates.iter().map(Vec::len).sum()),
                    reason: "allele preview limit".to_owned(),
                };
            }
        }
    }
    document.raw_fields = record
        .info
        .iter()
        .enumerate()
        .map(|(index, field)| {
            let value = format_variant_field(field);
            InspectorField::new(
                format!("info-{index}"),
                &field.key,
                InspectorValue::RawKeyValue {
                    key: field.key.clone(),
                    value: value.clone(),
                },
            )
            .with_copy_value(&value)
            .with_provenance(if field.truncated {
                InspectorProvenance::Truncated {
                    displayed: value.len(),
                    total: None,
                    reason: "VCF INFO resource policy".to_owned(),
                }
            } else {
                InspectorProvenance::Source
            })
        })
        .collect();

    let mut genotype_indices = (0..record.genotypes.len()).collect::<Vec<_>>();
    let mut missing_pedigree_order = false;
    match inspector.settings.genotype_grouping {
        VariantGenotypeGrouping::SampleOrder => {}
        VariantGenotypeGrouping::Genotype => {
            genotype_indices.sort_by_key(|index| {
                let genotype = &record.genotypes[*index];
                (variant_genotype_group_rank(genotype), genotype.sample_index)
            });
        }
        VariantGenotypeGrouping::Pedigree => {
            if let Some(order) = sample_metadata.and_then(|metadata| {
                metadata.pedigree_sample_order(&inspector.query.header.samples)
            }) {
                let mut rank = vec![usize::MAX; inspector.query.header.samples.len()];
                for (position, sample_index) in order.into_iter().enumerate() {
                    if let Some(rank) = rank.get_mut(sample_index) {
                        *rank = position;
                    }
                }
                genotype_indices.sort_by_key(|index| {
                    let sample_index = record.genotypes[*index].sample_index;
                    (
                        rank.get(sample_index).copied().unwrap_or(usize::MAX),
                        sample_index,
                    )
                });
            } else {
                missing_pedigree_order = true;
            }
        }
    }
    let normalized_search = sample_search
        .map(str::trim)
        .filter(|search| !search.is_empty())
        .map(str::to_ascii_lowercase);
    if let Some(search) = &normalized_search {
        genotype_indices.retain(|index| {
            inspector
                .query
                .header
                .samples
                .get(record.genotypes[*index].sample_index)
                .is_some_and(|sample| sample.to_ascii_lowercase().contains(search))
        });
    }
    let matching_genotypes = genotype_indices.len();
    let presented_rows = genotype_indices
        .into_iter()
        .take(MAX_PRESENTED_GENOTYPE_ROWS)
        .map(|index| {
            let genotype = &record.genotypes[index];
            let sample = inspector
                .query
                .header
                .samples
                .get(genotype.sample_index)
                .cloned()
                .unwrap_or_else(|| "?".to_owned());
            let fields = genotype
                .fields
                .iter()
                .map(|field| format!("{}={}", field.key, format_variant_field(field)))
                .collect::<Vec<_>>()
                .join("; ");
            vec![
                sample,
                variant_genotype_group_label(genotype).to_owned(),
                format_genotype(genotype),
                fields,
            ]
        })
        .collect::<Vec<_>>();
    if missing_pedigree_order {
        document.notices.push(InspectorNotice {
            id: "missing-pedigree-order".to_owned(),
            tone: InspectorTone::Warning,
            message: "Pedigree order requires imported father_id or mother_id sample attributes; retaining VCF header order.".to_owned(),
        });
    }
    if !record.genotypes.is_empty() {
        document.sections.push(
            InspectorSection::new(
                "genotypes",
                format!(
                    "Genotypes · {} loaded / {} selected / {} in file",
                    record.genotypes.len(),
                    inspector.query.provenance.selected_samples,
                    inspector.query.provenance.total_samples,
                ),
                100,
            )
            .collapsed()
            .with_fields(vec![InspectorField::new(
                "genotype-table",
                "Samples",
                InspectorValue::Table(InspectorTable {
                    columns: vec![
                        "Sample".to_owned(),
                        "Group".to_owned(),
                        "Genotype".to_owned(),
                        "FORMAT".to_owned(),
                    ],
                    total_rows: matching_genotypes,
                    omitted_rows: matching_genotypes.saturating_sub(presented_rows.len()),
                    rows: presented_rows,
                    search_label: Some("Filter loaded samples".to_owned()),
                }),
            )]),
        );
    }
    let provenance = inspector.query.provenance;
    if provenance.genotype_cells_omitted > 0
        || provenance.records_truncated
        || provenance.header_samples_omitted > 0
        || provenance.info_fields_truncated > 0
        || provenance.format_fields_truncated > 0
    {
        document.notices.push(InspectorNotice {
            id: "vcf-resource-bounds".to_owned(),
            tone: InspectorTone::Warning,
            message: format!(
                "Bounded VCF query: {} genotype cells omitted · {} header samples omitted · {} INFO schemas omitted · {} FORMAT schemas omitted{}",
                provenance.genotype_cells_omitted,
                provenance.header_samples_omitted,
                provenance.info_fields_truncated,
                provenance.format_fields_truncated,
                if provenance.records_truncated {
                    " · record limit reached"
                } else {
                    ""
                }
            ),
        });
    }
    document.actions = vec![
        InspectorAction::new(InspectorActionId::CopyDetails, "Copy details").primary(),
        InspectorAction::new(
            InspectorActionId::ExportVariantGenotypes,
            "Export variant + genotypes TSV…",
        ),
        InspectorAction::new(
            InspectorActionId::ExportVariantEvidence,
            "Export alignment evidence TSV…",
        ),
        InspectorAction::new(InspectorActionId::ClearSelection, "Clear selection").low_emphasis(),
    ];
    for index in 0..record.breakends.len().min(16) {
        document.actions.push(InspectorAction::new(
            InspectorActionId::OpenVariantPartner(index),
            format!("Open partner {}", index + 1),
        ));
    }
    document.preserve_automation_fields(&legacy_fields);
    let copy_query = Arc::clone(&inspector.query);
    let copy_record_index = inspector.record_index;
    document.preserve_deferred_copy_details(
        format!(
            "variant-details:{}:{copy_record_index}",
            inspector.track_name
        ),
        move || {
            copy_query.records.get(copy_record_index).map_or_else(
                || inspector_fields_text(&legacy_fields),
                |record| variant_details_text(&legacy_fields, record),
            )
        },
    );
    document
}

/// Adds runtime alignment context and navigation availability to the
/// source-derived VCF document. Keeping this assembly in the adapter prevents
/// the shared Properties renderer from acquiring VCF-specific branches.
#[allow(clippy::too_many_lines)]
pub(crate) fn variant_inspector_presentation_document(
    inspector: &VariantInspectorSnapshot,
    record: &rgv_hts::VariantRecord,
    alignment_evidence: &[VariantAlignmentEvidence],
    sample_metadata: Option<&SampleMetadataTable>,
    sample_search: &str,
    available_panel_slots: usize,
) -> InspectorDocument {
    let mut document = variant_inspector_document_with_metadata(
        inspector,
        record,
        sample_metadata,
        Some(sample_search),
    );
    if !record.breakends.is_empty() {
        document.sections.push(
            InspectorSection::new("breakend-partners", "Breakend partners", 30)
                .collapsed()
                .with_fields(vec![InspectorField::new(
                    "breakend-list",
                    "Partners",
                    InspectorValue::List(
                        record
                            .breakends
                            .iter()
                            .take(16)
                            .map(|partner| {
                                format!(
                                    "ALT {}: local join {} to {}:{} partner join {}",
                                    partner.alternate_index + 1,
                                    breakend_join_side(partner.local_join_after_position),
                                    partner.contig,
                                    partner.position + 1,
                                    breakend_join_side(partner.partner_join_after_position),
                                )
                            })
                            .collect(),
                    ),
                )]),
        );
    }
    if !record.breakpoint_orientations.is_empty() {
        document.sections.push(
            InspectorSection::new("breakpoint-orientations", "Breakpoint orientations", 40)
                .collapsed()
                .with_fields(vec![InspectorField::new(
                    "orientation-list",
                    "Orientations",
                    InspectorValue::List(
                        record
                            .breakpoint_orientations
                            .iter()
                            .take(16)
                            .map(|orientation| {
                                format!(
                                    "local join {} to partner join {}{}",
                                    breakend_join_side(orientation.local_join_after_position),
                                    breakend_join_side(orientation.partner_join_after_position),
                                    orientation.support.map_or_else(String::new, |support| {
                                        format!(" · support {support}")
                                    }),
                                )
                            })
                            .collect(),
                    ),
                )]),
        );
    }
    if alignment_evidence.is_empty() {
        document.notices.push(InspectorNotice {
            id: "no-alignment-evidence".to_owned(),
            tone: InspectorTone::Neutral,
            message: "No alignment query currently covers this variant position.".to_owned(),
        });
    } else {
        document.sections.push(
            InspectorSection::new("alignment-evidence", "Alignment evidence", 50)
                .collapsed()
                .with_fields(vec![
                    InspectorField::new(
                        "alignment-evidence-table",
                        "Observed context",
                        InspectorValue::Table(InspectorTable {
                            columns: vec![
                                "Track".to_owned(),
                                "Depth".to_owned(),
                                "Alleles".to_owned(),
                                "Modifications".to_owned(),
                            ],
                            rows: alignment_evidence
                                .iter()
                                .map(|evidence| {
                                    vec![
                                        evidence.track_name.clone(),
                                        format!(
                                            "{}× ({} forward · {} reverse)",
                                            evidence.depth,
                                            evidence.forward_depth,
                                            evidence.reverse_depth
                                        ),
                                        evidence.allele_summary.clone(),
                                        format!(
                                            "{} calls / {} series",
                                            evidence.modification_calls,
                                            evidence.modification_series
                                        ),
                                    ]
                                })
                                .collect(),
                            total_rows: alignment_evidence.len(),
                            omitted_rows: 0,
                            search_label: None,
                        }),
                    )
                    .with_provenance(InspectorProvenance::Aggregated(
                        "observations at the selected coordinate; not an RGV variant call"
                            .to_owned(),
                    )),
                ]),
        );
        for evidence in alignment_evidence {
            document.actions.push(InspectorAction::new(
                InspectorActionId::InspectVariantPileup(evidence.track_id, evidence.position),
                format!("Inspect {} pileup", evidence.track_name),
            ));
            let modifications = InspectorAction::new(
                InspectorActionId::InspectVariantModifications(
                    evidence.track_id,
                    evidence.position,
                ),
                format!("Inspect {} modifications", evidence.track_name),
            );
            document.actions.push(if evidence.modification_calls > 0 {
                modifications
            } else {
                modifications.disabled("No modification calls occur at this position")
            });
        }
    }
    if record.end > record.start + 1 {
        let endpoint = InspectorAction::new(
            InspectorActionId::OpenVariantEndpoint,
            "Open event end for pair/split review",
        );
        document.actions.push(if available_panel_slots > 0 {
            endpoint
        } else {
            endpoint.disabled("The workspace already has the maximum number of panels")
        });
    }
    for action in &mut document.actions {
        if matches!(action.id, InspectorActionId::OpenVariantPartner(_))
            && available_panel_slots == 0
        {
            *action = action
                .clone()
                .disabled("The workspace already has the maximum number of panels");
        }
        if action.id == InspectorActionId::ExportVariantEvidence && alignment_evidence.is_empty() {
            *action = action
                .clone()
                .disabled("No alignment evidence is available to export");
        }
    }
    document
}

fn variant_inspector_legacy_fields(
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
    bin: &rgv_hts::PileupBin,
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
    for (index, partner) in record.breakends.iter().enumerate() {
        let _ = write!(
            details,
            "\nBreakend ALT {}: local join {} -> {}:{} partner join {}",
            partner.alternate_index + 1,
            breakend_join_side(partner.local_join_after_position),
            partner.contig,
            partner.position + 1,
            breakend_join_side(partner.partner_join_after_position),
        );
        if index + 1 >= 16 {
            break;
        }
    }
    for (index, orientation) in record.breakpoint_orientations.iter().enumerate() {
        let _ = write!(
            details,
            "\nBreakpoint orientation {}: local join {} -> partner join {}{}",
            index + 1,
            breakend_join_side(orientation.local_join_after_position),
            breakend_join_side(orientation.partner_join_after_position),
            orientation
                .support
                .map_or_else(String::new, |support| format!(" · support {support}")),
        );
        if index + 1 >= 16 {
            break;
        }
    }
    if details.len() > VARIANT_DETAILS_COPY_BYTES {
        truncate_string_to_bytes(&mut details, VARIANT_DETAILS_COPY_BYTES - 32);
        details.push_str("\n… clipboard limit reached");
    }
    details
}

pub(crate) const fn breakend_join_side(join_after_position: bool) -> &'static str {
    if join_after_position {
        "after"
    } else {
        "before"
    }
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
    if !record.breakends.is_empty() {
        if !push_variant_export_line(
            &mut output,
            "section\talternate\tlocal_join_side\tpartner_contig\tpartner_position_1based\tpartner_join_side",
        ) {
            return output;
        }
        for partner in &record.breakends {
            let line = format!(
                "breakend\t{}\t{}\t{}\t{}\t{}",
                partner.alternate_index + 1,
                breakend_join_side(partner.local_join_after_position),
                tsv_cell(&partner.contig),
                partner.position + 1,
                breakend_join_side(partner.partner_join_after_position),
            );
            if !push_variant_export_line(&mut output, &line) {
                return output;
            }
        }
    }
    if !record.breakpoint_orientations.is_empty() {
        if !push_variant_export_line(
            &mut output,
            "section\torientation\tlocal_join_side\tpartner_join_side\tsupport",
        ) {
            return output;
        }
        for (index, orientation) in record.breakpoint_orientations.iter().enumerate() {
            let line = format!(
                "breakpoint_orientation\t{}\t{}\t{}\t{}",
                index + 1,
                breakend_join_side(orientation.local_join_after_position),
                breakend_join_side(orientation.partner_join_after_position),
                orientation
                    .support
                    .map_or_else(|| ".".to_owned(), |support| support.to_string()),
            );
            if !push_variant_export_line(&mut output, &line) {
                return output;
            }
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
        "alignment_track\tbin_start_1based\tbin_end_1based_inclusive\tdepth\tforward_depth\treverse_depth\tallele_observations\tmodification_calls\tmodification_series\tproper_pair_model_samples\tcomplete_individual_records\tcomplete_individual_reads\tpileup_complete".to_owned(),
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
            item.complete_individual_reads,
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
