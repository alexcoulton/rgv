use super::super::*;

fn feature_attribute<'a>(record: &'a rgv_hts::FeatureRecord, keys: &[&str]) -> Option<&'a str> {
    record
        .attributes
        .iter()
        .find(|(key, _)| keys.iter().any(|candidate| key == candidate))
        .map(|(_, value)| value.as_str())
}

fn feature_attribute_values(record: &rgv_hts::FeatureRecord, key: &str) -> Vec<String> {
    record
        .attributes
        .iter()
        .filter(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.clone())
        .collect()
}

fn semantic_identifier_field(id: &str, label: &str, value: Option<&str>) -> Option<InspectorField> {
    value.map(|value| {
        InspectorField::new(id, label, InspectorValue::Identifier(value.to_owned()))
            .with_copy_value(value)
            .with_provenance(InspectorProvenance::Parsed)
    })
}

fn feature_tag_label(tag: &str) -> String {
    match tag {
        "basic" => "Basic".to_owned(),
        value if value.starts_with("appris_principal") => {
            let level = value
                .trim_start_matches("appris_principal")
                .trim_start_matches('_');
            if level.is_empty() {
                "APPRIS principal".to_owned()
            } else {
                format!("APPRIS principal {level}")
            }
        }
        value if value.starts_with("CCDS") => value.to_owned(),
        value => value.replace('_', " "),
    }
}

fn bounded_feature_attribute_display(value: &str) -> (String, bool) {
    if value.len() <= MAX_FEATURE_INSPECTOR_VALUE_BYTES {
        return (value.to_owned(), false);
    }
    let mut end = MAX_FEATURE_INSPECTOR_VALUE_BYTES.saturating_sub('…'.len_utf8());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (format!("{}…", &value[..end]), true)
}

fn complete_feature_details_text(
    legacy_fields: &[(String, String)],
    record: &rgv_hts::FeatureRecord,
) -> String {
    let core_end = legacy_fields
        .iter()
        .position(|(label, _)| label.starts_with("Attribute · "))
        .unwrap_or(legacy_fields.len());
    let mut details = inspector_fields_text(&legacy_fields[..core_end]);
    if record.blocks.len() > MAX_FEATURE_INSPECTOR_BLOCKS {
        let _ = write!(details, "\nComplete blocks ({})", record.blocks.len());
        for (index, (start, end)) in record.blocks.iter().enumerate() {
            let _ = write!(details, "\nBlock {}: {}-{}", index + 1, start + 1, end);
        }
    }
    for (index, (key, value)) in record.attributes.iter().enumerate() {
        let _ = write!(details, "\nAttribute {} · {key}: {value}", index + 1);
    }
    details
}

fn add_standard_document_actions(document: &mut InspectorDocument) {
    document.actions.extend([
        InspectorAction::new(InspectorActionId::CopyDetails, "Copy details").primary(),
        InspectorAction::new(InspectorActionId::ClearSelection, "Clear selection").low_emphasis(),
    ]);
}

fn take_inspector_fields(
    fields: &mut Vec<InspectorField>,
    mut predicate: impl FnMut(&InspectorField) -> bool,
) -> Vec<InspectorField> {
    let mut selected = Vec::new();
    let mut remaining = Vec::new();
    for field in std::mem::take(fields) {
        if predicate(&field) {
            selected.push(field);
        } else {
            remaining.push(field);
        }
    }
    *fields = remaining;
    selected
}

#[allow(clippy::too_many_lines)]
pub(crate) fn feature_inspector_fields(
    inspector: &FeatureInspectorSnapshot,
) -> Vec<(String, String)> {
    feature_inspector_document(inspector).flatten_automation_fields()
}

#[allow(clippy::too_many_lines)]
pub(crate) fn feature_inspector_document(
    inspector: &FeatureInspectorSnapshot,
) -> InspectorDocument {
    let record = &inspector.record;
    let legacy_fields = feature_inspector_legacy_fields(inspector);
    let gene_name = feature_attribute(record, &["gene_name"]);
    let transcript_name = feature_attribute(record, &["transcript_name"]);
    let title = gene_name
        .or(transcript_name)
        .map(ToOwned::to_owned)
        .or_else(|| record.name.clone())
        .unwrap_or_else(|| {
            record
                .feature_type
                .clone()
                .unwrap_or_else(|| "Feature".to_owned())
        });
    let feature_type = record.feature_type.as_deref().unwrap_or("Feature");
    let exon_number = feature_attribute(record, &["exon_number"]);
    let subtitle = transcript_name.map_or_else(
        || feature_type.to_owned(),
        |transcript| {
            exon_number.map_or_else(
                || format!("{feature_type} · {transcript}"),
                |exon| format!("{feature_type} · {transcript} · exon {exon}"),
            )
        },
    );
    let strand = match record.strand {
        rgv_hts::Strand::Forward => "+ strand",
        rgv_hts::Strand::Reverse => "− strand",
        rgv_hts::Strand::Unstranded => "unstranded",
    };
    let length = record.end.saturating_sub(record.start);
    let location_display = format!(
        "{}:{}–{} · {strand} · {length} bp",
        record.contig,
        record.start.saturating_add(1),
        record.end,
    );
    let mut document = InspectorDocument::new(
        format!(
            "feature:{}:{}:{}:{}",
            inspector.track_name, record.contig, record.start, record.end
        ),
        title,
    );
    document.header.subtitle = Some(subtitle);
    document.header.location = Some(InspectorLocation {
        contig: record.contig.clone(),
        start: record.start,
        end: record.end,
        strand: Some(record.strand),
        display: location_display,
    });
    document.header.source = Some(inspector.track_name.clone());
    document.header.badges.push(InspectorBadge {
        label: feature_type.to_owned(),
        tone: InspectorTone::Informative,
    });

    let mut overview = InspectorSection::new("overview", "Overview", 0);
    overview.fields.push(InspectorField::new(
        "track",
        "Track",
        InspectorValue::Text(inspector.track_name.clone()),
    ));
    if let Some(score) = record.score {
        overview.fields.push(InspectorField::new(
            "score",
            "Score",
            InspectorValue::Number {
                display: score.to_string(),
                value: Some(f64::from(score)),
            },
        ));
    }
    if let Some((start, end)) = record.coding_interval {
        overview.fields.push(InspectorField::new(
            "coding-interval",
            "Coding sequence",
            InspectorValue::GenomicLocation(InspectorLocation {
                contig: record.contig.clone(),
                start,
                end,
                strand: Some(record.strand),
                display: format!("{}:{}–{}", record.contig, start + 1, end),
            }),
        ));
    }
    if !record.blocks.is_empty() {
        overview.fields.push(InspectorField::new(
            "exon-count",
            "Exon count",
            InspectorValue::Number {
                display: record.blocks.len().to_string(),
                value: Some(record.blocks.len() as f64),
            },
        ));
        let blocks = record
            .blocks
            .iter()
            .take(MAX_FEATURE_INSPECTOR_BLOCKS)
            .map(|(start, end)| format!("{}–{}", start + 1, end))
            .collect::<Vec<_>>();
        overview.fields.push(
            InspectorField::new(
                "blocks",
                if feature_type == "PSL alignment" {
                    "Alignment blocks"
                } else {
                    "Blocks / exons"
                },
                InspectorValue::List(blocks),
            )
            .with_provenance(if record.blocks.len() > MAX_FEATURE_INSPECTOR_BLOCKS {
                InspectorProvenance::Truncated {
                    displayed: MAX_FEATURE_INSPECTOR_BLOCKS,
                    total: Some(record.blocks.len()),
                    reason: "panel block limit".to_owned(),
                }
            } else {
                InspectorProvenance::Source
            }),
        );
    }
    if feature_type == "transcript" {
        let model_status =
            record
                .coding_interval
                .map_or("Non-coding model", |(coding_start, coding_end)| {
                    if record
                        .blocks
                        .iter()
                        .any(|(start, end)| *start < coding_start || *end > coding_end)
                    {
                        "Coding model with UTR segments"
                    } else {
                        "Coding model"
                    }
                });
        overview.fields.push(
            InspectorField::new(
                "coding-status",
                "CDS / UTR status",
                InspectorValue::Status {
                    label: model_status.to_owned(),
                    tone: InspectorTone::Informative,
                },
            )
            .with_provenance(InspectorProvenance::Parsed),
        );
    }
    document.sections.push(overview);

    let gene_fields = [
        semantic_identifier_field("gene-name", "Gene", gene_name),
        semantic_identifier_field(
            "gene-id",
            "Stable identifier",
            feature_attribute(record, &["gene_id"]),
        ),
        feature_attribute(record, &["gene_type", "gene_biotype"]).map(|value| {
            InspectorField::new(
                "gene-biotype",
                "Biotype",
                InspectorValue::Status {
                    label: value.replace('_', " "),
                    tone: InspectorTone::Neutral,
                },
            )
            .with_provenance(InspectorProvenance::Parsed)
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !gene_fields.is_empty() {
        document
            .sections
            .push(InspectorSection::new("gene", "Gene", 10).with_fields(gene_fields));
    }

    let transcript_fields = [
        semantic_identifier_field("transcript-name", "Transcript", transcript_name),
        semantic_identifier_field(
            "transcript-id",
            "Stable identifier",
            feature_attribute(record, &["transcript_id"]),
        ),
        feature_attribute(record, &["transcript_type", "transcript_biotype"]).map(|value| {
            InspectorField::new(
                "transcript-biotype",
                "Biotype",
                InspectorValue::Status {
                    label: value.replace('_', " "),
                    tone: InspectorTone::Neutral,
                },
            )
            .with_provenance(InspectorProvenance::Parsed)
        }),
        feature_attribute(record, &["transcript_support_level"]).map(|value| {
            InspectorField::new(
                "transcript-support-level",
                "Support level",
                InspectorValue::Status {
                    label: value.to_owned(),
                    tone: InspectorTone::Positive,
                },
            )
            .with_provenance(InspectorProvenance::Parsed)
        }),
        feature_attribute(record, &["landmark_selection_policy"]).map(|value| {
            InspectorField::new(
                "representative-selection",
                "Representative selection",
                InspectorValue::Text(value.to_owned()),
            )
            .with_provenance(InspectorProvenance::Parsed)
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if !transcript_fields.is_empty() {
        document.sections.push(
            InspectorSection::new("transcript", "Transcript", 20).with_fields(transcript_fields),
        );
    }

    let mut context_fields = [
        semantic_identifier_field(
            "exon-id",
            "Exon identifier",
            feature_attribute(record, &["exon_id"]),
        ),
        semantic_identifier_field("exon-number", "Exon number", exon_number),
        semantic_identifier_field(
            "protein-id",
            "Protein identifier",
            feature_attribute(record, &["protein_id"]),
        ),
        semantic_identifier_field(
            "ccds-id",
            "CCDS",
            feature_attribute(record, &["ccdsid", "ccds_id"]),
        ),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if matches!(
        feature_type.to_ascii_lowercase().as_str(),
        "exon" | "cds" | "utr"
    ) {
        context_fields.insert(
            0,
            InspectorField::new(
                "feature-context-type",
                "Context",
                InspectorValue::Status {
                    label: feature_type.to_owned(),
                    tone: InspectorTone::Informative,
                },
            )
            .with_provenance(InspectorProvenance::Parsed),
        );
    }
    if !context_fields.is_empty()
        || matches!(
            feature_type.to_ascii_lowercase().as_str(),
            "exon" | "cds" | "utr"
        )
    {
        document.sections.push(
            InspectorSection::new("feature-context", "Feature context", 30)
                .with_fields(context_fields),
        );
    }

    let tags = feature_attribute_values(record, "tag")
        .into_iter()
        .map(|tag| feature_tag_label(&tag))
        .collect::<Vec<_>>();
    if !tags.is_empty() {
        document
            .sections
            .push(InspectorSection::new("tags", "Tags", 40).with_fields(vec![
                InspectorField::new("tags", "Tags", InspectorValue::Tags(tags))
                    .with_provenance(InspectorProvenance::Parsed),
            ]));
    }

    let source_fields = [
        ("source-assembly", "Assembly", "landmark_assembly"),
        (
            "source-provider",
            "Annotation provider",
            "landmark_provider",
        ),
        ("source-release", "Release", "landmark_release"),
        ("source-bundle", "Bundle", "landmark_bundle_id"),
        (
            "source-bundle-version",
            "Resource version",
            "landmark_bundle_version",
        ),
        ("source-format", "Asset format", "landmark_asset_format"),
        ("source-checksum", "Asset SHA-256", "landmark_asset_sha256"),
        ("source-citation", "Citation", "landmark_citation"),
        ("source-license", "Licence", "landmark_license"),
        (
            "source-license-url",
            "Citation / licence link",
            "landmark_license_url",
        ),
    ]
    .into_iter()
    .filter_map(|(id, label, key)| {
        feature_attribute(record, &[key]).map(|value| {
            InspectorField::new(id, label, InspectorValue::Text(value.to_owned()))
                .with_copy_value(value)
                .with_provenance(InspectorProvenance::Parsed)
        })
    })
    .collect::<Vec<_>>();
    if !source_fields.is_empty() {
        document.sections.push(
            InspectorSection::new("landmark-source", "Source and provenance", 50)
                .with_fields(source_fields),
        );
    }

    if let Some(peak) = record.peak {
        let mut peak_fields = vec![InspectorField::new(
            "signal-value",
            "Signal value",
            InspectorValue::Number {
                display: peak.signal_value.to_string(),
                value: Some(f64::from(peak.signal_value)),
            },
        )];
        for (id, label, value) in [
            ("peak-p-value", "p-value (−log10)", peak.p_value),
            ("peak-q-value", "q-value (−log10 FDR)", peak.q_value),
        ] {
            if let Some(value) = value {
                peak_fields.push(InspectorField::new(
                    id,
                    label,
                    InspectorValue::Number {
                        display: value.to_string(),
                        value: Some(f64::from(value)),
                    },
                ));
            }
        }
        if let Some(summit) = peak.summit {
            peak_fields.push(InspectorField::new(
                "peak-summit",
                "Called summit",
                InspectorValue::GenomicLocation(InspectorLocation {
                    contig: record.contig.clone(),
                    start: summit,
                    end: summit.saturating_add(1),
                    strand: Some(record.strand),
                    display: format!("{}:{}", record.contig, summit + 1),
                }),
            ));
        }
        document.sections.push(
            InspectorSection::new("peak-statistics", "Peak statistics", 35)
                .with_fields(peak_fields),
        );
    }

    document.raw_fields = record
        .attributes
        .iter()
        .take(MAX_FEATURE_INSPECTOR_ATTRIBUTES)
        .enumerate()
        .map(|(index, (key, complete_value))| {
            let (display_value, truncated) = bounded_feature_attribute_display(complete_value);
            InspectorField::new(
                format!("raw-attribute-{index}"),
                key,
                InspectorValue::RawKeyValue {
                    key: key.clone(),
                    value: display_value,
                },
            )
            .with_copy_value(complete_value)
            .with_provenance(if truncated {
                InspectorProvenance::Truncated {
                    displayed: MAX_FEATURE_INSPECTOR_VALUE_BYTES,
                    total: Some(complete_value.len()),
                    reason: "panel value limit".to_owned(),
                }
            } else {
                InspectorProvenance::Source
            })
        })
        .collect();
    if record.attributes.len() > MAX_FEATURE_INSPECTOR_ATTRIBUTES {
        document.notices.push(InspectorNotice {
            id: "raw-attribute-limit".to_owned(),
            tone: InspectorTone::Warning,
            message: format!(
                "{} raw attributes are available in the record; {} are shown because the panel is bounded.",
                record.attributes.len(),
                MAX_FEATURE_INSPECTOR_ATTRIBUTES,
            ),
        });
    }
    document.actions = vec![
        InspectorAction::new(InspectorActionId::CopyDetails, "Copy details").primary(),
        InspectorAction::new(
            InspectorActionId::AddFeatureToReview,
            "Add feature to review queue",
        ),
        InspectorAction::new(InspectorActionId::ClearSelection, "Clear selection").low_emphasis(),
    ];
    if feature_attribute(record, &["landmark_asset_role"]) == Some("representative_genes")
        && let Some(bundle_id) = feature_attribute(record, &["landmark_bundle_id"])
    {
        document.actions.insert(
            1,
            InspectorAction::new(
                InspectorActionId::ShowAllTranscriptsForGene(bundle_id.to_owned()),
                "Show all transcripts for this gene",
            ),
        );
    }
    document.preserve_automation_fields(&legacy_fields);
    if record.blocks.len() > MAX_FEATURE_INSPECTOR_BLOCKS
        || record.attributes.len() > MAX_FEATURE_INSPECTOR_ATTRIBUTES
        || record
            .attributes
            .iter()
            .any(|(_, value)| value.len() > MAX_FEATURE_INSPECTOR_VALUE_BYTES)
    {
        document.preserve_copy_details(complete_feature_details_text(&legacy_fields, record));
    }
    document
}

#[allow(clippy::too_many_lines)]
fn feature_inspector_legacy_fields(inspector: &FeatureInspectorSnapshot) -> Vec<(String, String)> {
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
        let (value, _) = bounded_feature_attribute_display(value);
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
    interaction_inspector_document(inspector).flatten_automation_fields()
}

pub(crate) fn interaction_inspector_document(
    inspector: &InteractionInspectorSnapshot,
) -> InspectorDocument {
    let record = &inspector.query.records[inspector.record_index];
    let legacy_fields = interaction_inspector_legacy_fields(inspector);
    let mut document = InspectorDocument::from_legacy_fields(
        format!(
            "interaction:{}:{}:{}:{}",
            inspector.track_name, record.source.contig, record.source.start, record.source.end
        ),
        record
            .name
            .clone()
            .unwrap_or_else(|| "Interaction".to_owned()),
        legacy_fields.clone(),
    );
    document.header.subtitle = Some(if record.directional {
        "Directed genomic interaction".to_owned()
    } else {
        "Undirected genomic interaction".to_owned()
    });
    document.header.source = Some(inspector.track_name.clone());
    document.header.badges.push(InspectorBadge {
        label: "Interaction".to_owned(),
        tone: InspectorTone::Informative,
    });
    let mut fields = std::mem::take(&mut document.sections[0].fields);
    let endpoints = take_inspector_fields(&mut fields, |field| {
        matches!(field.label.as_str(), "Source" | "Target")
    });
    let provenance = take_inspector_fields(&mut fields, |field| field.label == "Query provenance");
    let display = take_inspector_fields(&mut fields, |field| field.label == "Display provenance");
    document.sections = vec![
        InspectorSection::new("interaction-summary", "Interaction", 0).with_fields(fields),
        InspectorSection::new("interaction-endpoints", "Endpoints", 10).with_fields(endpoints),
        InspectorSection::new("interaction-display", "Display context", 30)
            .collapsed()
            .with_fields(display),
        InspectorSection::new("interaction-provenance", "Query provenance", 40)
            .collapsed()
            .with_fields(provenance),
    ];
    document.header.location = document
        .sections
        .iter()
        .flat_map(|section| &section.fields)
        .find_map(|field| match &field.value {
            InspectorValue::GenomicLocation(location) => Some(location.clone()),
            _ => None,
        });
    if inspector.query.provenance.records_truncated {
        document.notices.push(InspectorNotice {
            id: "interaction-query-bound".to_owned(),
            tone: InspectorTone::Warning,
            message: "The interaction query reached its record bound; this record is retained but the visible set is incomplete.".to_owned(),
        });
    }
    document.actions.extend([
        InspectorAction::new(
            InspectorActionId::OpenInteractionEndpoint(InspectorEndpoint::Source),
            "Open source endpoint",
        ),
        InspectorAction::new(
            InspectorActionId::OpenInteractionEndpoint(InspectorEndpoint::Target),
            "Open target endpoint",
        ),
    ]);
    add_standard_document_actions(&mut document);
    document.preserve_automation_fields(&legacy_fields);
    document
}

pub(crate) fn interaction_inspector_presentation_document(
    inspector: &InteractionInspectorSnapshot,
    available_panel_slots: usize,
) -> InspectorDocument {
    let mut document = interaction_inspector_document(inspector);
    if available_panel_slots == 0 {
        for action in &mut document.actions {
            if matches!(action.id, InspectorActionId::OpenInteractionEndpoint(_)) {
                *action = action
                    .clone()
                    .disabled("The workspace already has the maximum number of panels");
            }
        }
    }
    document
}

fn interaction_inspector_legacy_fields(
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
                "source to target"
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
    quantitative_inspector_document(inspector).flatten_automation_fields()
}

pub(crate) fn quantitative_inspector_document(
    inspector: &QuantitativeInspectorSnapshot,
) -> InspectorDocument {
    let record = &inspector.query.records[inspector.record_index];
    let legacy_fields = quantitative_inspector_legacy_fields(inspector);
    let mut document = InspectorDocument::from_legacy_fields(
        format!(
            "quantitative:{}:{}:{}",
            inspector.track_name, inspector.record_index, inspector.selected_position
        ),
        "Quantitative value",
        legacy_fields.clone(),
    );
    document.header.subtitle = Some(format!(
        "Source interval {}–{}",
        record.start.saturating_add(1),
        record.end
    ));
    document.header.source = Some(inspector.track_name.clone());
    let mut fields = std::mem::take(&mut document.sections[0].fields);
    for field in &mut fields {
        if field.label == "Source value" {
            if !matches!(field.value, InspectorValue::Missing { .. }) {
                field.provenance = InspectorProvenance::Source;
            }
        } else if field.label == "Derived value" {
            field.provenance = if matches!(field.value, InspectorValue::Missing { .. }) {
                InspectorProvenance::Missing(
                    "the requested arithmetic result could not be computed".to_owned(),
                )
            } else {
                InspectorProvenance::Aggregated(
                    "computed from the selected arithmetic operand".to_owned(),
                )
            };
        } else if matches!(field.label.as_str(), "Displayed scale" | "Window function") {
            field.provenance = InspectorProvenance::Aggregated(
                "display transformation; source value is retained separately".to_owned(),
            );
        }
    }
    let source = take_inspector_fields(&mut fields, |field| {
        matches!(
            field.label.as_str(),
            "Track" | "Selected position" | "Source interval" | "Source value"
        )
    });
    let derived = take_inspector_fields(&mut fields, |field| {
        matches!(
            field.label.as_str(),
            "Arithmetic" | "Operand value" | "Derived value"
        )
    });
    let provenance = take_inspector_fields(&mut fields, |field| {
        matches!(
            field.label.as_str(),
            "Window provenance" | "Query provenance"
        )
    });
    document.sections =
        vec![InspectorSection::new("quantitative-source", "Source value", 0).with_fields(source)];
    if !derived.is_empty() {
        document.sections.push(
            InspectorSection::new("quantitative-derived", "Derived value", 10).with_fields(derived),
        );
    }
    document.sections.push(
        InspectorSection::new("quantitative-display", "Display transformation", 20)
            .collapsed()
            .with_fields(fields),
    );
    document.sections.push(
        InspectorSection::new("quantitative-provenance", "Provenance", 30)
            .collapsed()
            .with_fields(provenance),
    );
    if inspector.query.provenance.records_truncated {
        document.notices.push(InspectorNotice {
            id: "quantitative-query-bound".to_owned(),
            tone: InspectorTone::Warning,
            message: "The quantitative query reached its source-record bound; the selected source interval remains exact.".to_owned(),
        });
    }
    add_standard_document_actions(&mut document);
    document.preserve_automation_fields(&legacy_fields);
    document
}

#[allow(clippy::too_many_lines)]
fn quantitative_inspector_legacy_fields(
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
    gwas_inspector_document(inspector).flatten_automation_fields()
}

pub(crate) fn gwas_inspector_document(inspector: &GwasInspectorSnapshot) -> InspectorDocument {
    let record = &inspector.query.records[inspector.record_index];
    let legacy_fields = gwas_inspector_legacy_fields(inspector);
    let mut document = InspectorDocument::from_legacy_fields(
        format!(
            "gwas:{}:{}:{}",
            inspector.track_name, record.contig, record.position
        ),
        &record.identifier,
        legacy_fields.clone(),
    );
    document.header.subtitle = Some("GWAS association".to_owned());
    document.header.source = Some(inspector.track_name.clone());
    document.header.location = Some(InspectorLocation {
        contig: record.contig.clone(),
        start: record.position,
        end: record.position.saturating_add(1),
        strand: None,
        display: format!("{}:{}", record.contig, record.position + 1),
    });
    document.raw_fields = record
        .fields
        .iter()
        .enumerate()
        .map(|(index, (key, value))| {
            InspectorField::new(
                format!("source-column-{index}"),
                key,
                InspectorValue::RawKeyValue {
                    key: key.clone(),
                    value: value.clone(),
                },
            )
            .with_copy_value(value)
        })
        .collect();
    let mut fields = std::mem::take(&mut document.sections[0].fields);
    let provenance = take_inspector_fields(&mut fields, |field| field.label == "Query provenance");
    let display = take_inspector_fields(&mut fields, |field| field.label == "Display");
    fields.retain(|field| !field.label.starts_with("Source column · "));
    document.sections = vec![
        InspectorSection::new("gwas-association", "Association", 0).with_fields(fields),
        InspectorSection::new("gwas-display", "Display context", 20)
            .collapsed()
            .with_fields(display),
        InspectorSection::new("gwas-provenance", "Query provenance", 30)
            .collapsed()
            .with_fields(provenance),
    ];
    if inspector.query.provenance.records_truncated {
        document.notices.push(InspectorNotice {
            id: "gwas-query-bound".to_owned(),
            tone: InspectorTone::Warning,
            message: "The GWAS query applied its positional strongest-point bound; the selected record retains all loaded source columns.".to_owned(),
        });
    }
    add_standard_document_actions(&mut document);
    document.preserve_automation_fields(&legacy_fields);
    document
}

fn gwas_inspector_legacy_fields(inspector: &GwasInspectorSnapshot) -> Vec<(String, String)> {
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
    copy_number_inspector_document(inspector).flatten_automation_fields()
}

pub(crate) fn copy_number_inspector_document(
    inspector: &CopyNumberInspectorSnapshot,
) -> InspectorDocument {
    let legacy_fields = copy_number_inspector_legacy_fields(inspector);
    let segment = &inspector.query.segments[inspector.segment_index];
    let sample = inspector
        .query
        .samples
        .get(segment.sample_index)
        .map_or("Copy number", String::as_str);
    let mut document = InspectorDocument::from_legacy_fields(
        format!(
            "copy-number:{}:{}",
            inspector.track_name, inspector.segment_index
        ),
        sample,
        legacy_fields.clone(),
    );
    document.header.source = Some(inspector.track_name.clone());
    document.header.badges.push(InspectorBadge {
        label: "Copy number".to_owned(),
        tone: InspectorTone::Informative,
    });
    let mut fields = std::mem::take(&mut document.sections[0].fields);
    if inspector.settings.value_mode == CopyNumberValueMode::Automatic
        && let Some(field) = fields
            .iter_mut()
            .find(|field| field.label == "Value interpretation")
    {
        field.provenance = InspectorProvenance::Inferred(
            "value semantics inferred from the loaded source".to_owned(),
        );
    }
    let provenance = take_inspector_fields(&mut fields, |field| field.label == "Query provenance");
    let ordering = take_inspector_fields(&mut fields, |field| {
        matches!(field.label.as_str(), "Row order" | "Sort locus")
    });
    document.sections = vec![
        InspectorSection::new("copy-number-segment", "Segment", 0).with_fields(fields),
        InspectorSection::new("copy-number-order", "Display order", 20)
            .collapsed()
            .with_fields(ordering),
        InspectorSection::new("copy-number-provenance", "Query provenance", 30)
            .collapsed()
            .with_fields(provenance),
    ];
    if inspector.query.provenance.records_truncated
        || inspector.query.provenance.sort_records_truncated
    {
        document.notices.push(InspectorNotice {
            id: "copy-number-query-bound".to_owned(),
            tone: InspectorTone::Warning,
            message: "The copy-number query or sort evidence reached a resource bound; the selected segment value remains exact.".to_owned(),
        });
    }
    add_standard_document_actions(&mut document);
    document.preserve_automation_fields(&legacy_fields);
    document
}

fn copy_number_inspector_legacy_fields(
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
    mutation_inspector_document(inspector).flatten_automation_fields()
}

#[allow(clippy::too_many_lines)]
pub(crate) fn mutation_inspector_document(
    inspector: &MutationInspectorSnapshot,
) -> InspectorDocument {
    let record = &inspector.query.records[inspector.record_index];
    let legacy_fields = mutation_inspector_legacy_fields(inspector);
    let mut document = InspectorDocument::from_legacy_fields(
        format!(
            "mutation:{}:{}:{}",
            inspector.track_name, inspector.contig, inspector.record_index
        ),
        if record.gene.is_empty() {
            "Mutation"
        } else {
            &record.gene
        },
        legacy_fields.clone(),
    );
    document.header.subtitle = Some(format!(
        "{} · {}",
        record.classification, record.variant_type
    ));
    document.header.source = Some(inspector.track_name.clone());
    document.header.location = Some(InspectorLocation {
        contig: inspector.contig.clone(),
        start: record.start,
        end: record.end.max(record.start.saturating_add(1)),
        strand: None,
        display: if record.insertion_boundary {
            format!(
                "{}: insertion boundary {}–{}",
                inspector.contig, record.source_start, record.source_end
            )
        } else {
            format!(
                "{}:{}–{}",
                inspector.contig, record.source_start, record.source_end
            )
        },
    });
    document.header.badges.push(InspectorBadge {
        label: "Mutation".to_owned(),
        tone: InspectorTone::Informative,
    });
    document.raw_fields = record
        .annotations
        .iter()
        .enumerate()
        .map(|(index, annotation)| {
            let key = inspector
                .query
                .headers
                .get(usize::from(annotation.column_index))
                .cloned()
                .unwrap_or_else(|| format!("MAF column {}", annotation.column_index + 1));
            InspectorField::new(
                format!("maf-annotation-{index}"),
                &key,
                InspectorValue::RawKeyValue {
                    key: key.clone(),
                    value: annotation.value.clone(),
                },
            )
            .with_copy_value(&annotation.value)
            .with_provenance(if annotation.truncated {
                InspectorProvenance::Truncated {
                    displayed: annotation.value.len(),
                    total: None,
                    reason: "source annotation resource limit".to_owned(),
                }
            } else {
                InspectorProvenance::Source
            })
        })
        .collect();
    let mut fields = std::mem::take(&mut document.sections[0].fields);
    let provenance = take_inspector_fields(&mut fields, |field| field.label == "Query provenance");
    fields.retain(|field| {
        !field.label.starts_with("MAF · ") && !field.label.starts_with("MAF column ")
    });
    document.sections = vec![
        InspectorSection::new("mutation-summary", "Mutation", 0).with_fields(fields),
        InspectorSection::new("mutation-provenance", "Query provenance", 30)
            .collapsed()
            .with_fields(provenance),
    ];
    let omitted_annotations = record.annotation_values_omitted;
    let provenance = inspector.query.provenance;
    if omitted_annotations > 0
        || provenance.header_columns_omitted > 0
        || provenance.annotation_values_truncated > 0
        || provenance.records_truncated
    {
        document.notices.push(InspectorNotice {
            id: "mutation-resource-bounds".to_owned(),
            tone: InspectorTone::Warning,
            message: format!(
                "Bounded mutation evidence: {omitted_annotations} record annotations omitted · {} header columns omitted · {} values truncated{}",
                provenance.header_columns_omitted,
                provenance.annotation_values_truncated,
                if provenance.records_truncated {
                    " · record limit reached"
                } else {
                    ""
                }
            ),
        });
    }
    add_standard_document_actions(&mut document);
    document.preserve_automation_fields(&legacy_fields);
    document
}

fn mutation_inspector_legacy_fields(
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
