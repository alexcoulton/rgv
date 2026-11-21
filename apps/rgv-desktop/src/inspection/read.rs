use crate::inspection::document::{
    InspectorAction, InspectorActionId, InspectorBadge, InspectorDocument, InspectorField,
    InspectorProvenance, InspectorSection, InspectorTone, InspectorValue,
};
use crate::inspection::model::{BoundedSequenceCopy, InspectorSnapshot};
use crate::{
    AUXILIARY_TAGS_COPY_BYTES, AUXILIARY_VALUE_DISPLAY_BYTES, EVENT_SEQUENCE_COPY_BASES,
    EVENT_SEQUENCE_PREVIEW_BASES, SEQUENCE_COPY_BASES,
};
use rgv_hts::{
    AlignedBaseClassification, AlignmentRecord, AuxValue, CigarOpKind, ReferenceSlice,
    SequenceData, classify_aligned_base,
};
use std::borrow::Cow;
use std::fmt::Write as _;

pub(crate) struct ReadEventDetails {
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) copy_sequence: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubstitutionCountSource {
    Nm,
    Md,
    ExplicitCigar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AlignmentEditSummary {
    pub(crate) substitutions: Option<u64>,
    pub(crate) substitution_source: Option<SubstitutionCountSource>,
    pub(crate) insertion_bases: u64,
    pub(crate) insertion_events: u64,
    pub(crate) deletion_bases: u64,
    pub(crate) deletion_events: u64,
    pub(crate) edit_distance: Option<u64>,
}

pub(crate) fn alignment_edit_summary(record: &AlignmentRecord) -> AlignmentEditSummary {
    let mut insertion_bases = 0_u64;
    let mut insertion_events = 0_u64;
    let mut deletion_bases = 0_u64;
    let mut deletion_events = 0_u64;
    let mut explicit_substitutions = 0_u64;
    let mut has_explicit_substitutions = false;
    for operation in &record.cigar {
        match operation.kind {
            CigarOpKind::Insertion => {
                insertion_bases = insertion_bases.saturating_add(u64::from(operation.len));
                insertion_events = insertion_events.saturating_add(1);
            }
            CigarOpKind::Deletion => {
                deletion_bases = deletion_bases.saturating_add(u64::from(operation.len));
                deletion_events = deletion_events.saturating_add(1);
            }
            CigarOpKind::SequenceMismatch => {
                explicit_substitutions =
                    explicit_substitutions.saturating_add(u64::from(operation.len));
                has_explicit_substitutions = true;
            }
            CigarOpKind::Match
            | CigarOpKind::SequenceMatch
            | CigarOpKind::ReferenceSkip
            | CigarOpKind::SoftClip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {}
        }
    }

    let edit_distance = record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == *b"NM")
        .and_then(|field| match field.value {
            AuxValue::Signed(value) => u64::try_from(value).ok(),
            AuxValue::Unsigned(value) => Some(value),
            _ => None,
        });
    let md_substitutions = record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == *b"MD")
        .and_then(|field| match &field.value {
            AuxValue::String(value) => md_substitution_count(value),
            _ => None,
        });
    let indel_bases = insertion_bases.saturating_add(deletion_bases);
    let (substitutions, substitution_source) = if let Some(substitutions) =
        edit_distance.and_then(|distance| distance.checked_sub(indel_bases))
    {
        (Some(substitutions), Some(SubstitutionCountSource::Nm))
    } else if let Some(substitutions) = md_substitutions {
        (Some(substitutions), Some(SubstitutionCountSource::Md))
    } else if has_explicit_substitutions {
        (
            Some(explicit_substitutions),
            Some(SubstitutionCountSource::ExplicitCigar),
        )
    } else {
        (None, None)
    };

    AlignmentEditSummary {
        substitutions,
        substitution_source,
        insertion_bases,
        insertion_events,
        deletion_bases,
        deletion_events,
        edit_distance,
    }
}

fn md_substitution_count(md: &str) -> Option<u64> {
    if md.is_empty() {
        return None;
    }
    let mut substitutions = 0_u64;
    let mut deletion = false;
    for byte in md.bytes() {
        match byte {
            b'0'..=b'9' => deletion = false,
            b'^' if !deletion => deletion = true,
            b'A'..=b'Z' | b'a'..=b'z' => {
                if !deletion {
                    substitutions = substitutions.saturating_add(1);
                }
            }
            _ => return None,
        }
    }
    Some(substitutions)
}

pub(crate) fn alignment_edit_fields(record: &AlignmentRecord) -> Vec<(String, String)> {
    let summary = alignment_edit_summary(record);
    let substitutions = match (summary.substitutions, summary.substitution_source) {
        (Some(count), Some(SubstitutionCountSource::Nm)) => {
            format!("{count} (derived from NM minus inserted and deleted bases)")
        }
        (Some(count), Some(SubstitutionCountSource::Md)) => format!("{count} (from MD)"),
        (Some(count), Some(SubstitutionCountSource::ExplicitCigar)) => {
            format!("{count} (from explicit CIGAR X operations)")
        }
        _ => "unavailable — requires NM, MD, explicit CIGAR X, or reference comparison".to_owned(),
    };
    vec![
        ("Base substitutions".to_owned(), substitutions),
        (
            "Insertions".to_owned(),
            format!(
                "{} bases across {} events",
                summary.insertion_bases, summary.insertion_events
            ),
        ),
        (
            "Deletions".to_owned(),
            format!(
                "{} bases across {} events",
                summary.deletion_bases, summary.deletion_events
            ),
        ),
        (
            "Edit distance (NM)".to_owned(),
            summary
                .edit_distance
                .map_or_else(|| "missing".to_owned(), |value| value.to_string()),
        ),
    ]
}

#[allow(clippy::too_many_lines)]
pub(crate) fn read_event_details(
    record: &AlignmentRecord,
    operation_index: usize,
    operation_offset: Option<u32>,
    reference: Option<&ReferenceSlice>,
    contig_name: &str,
) -> Option<ReadEventDetails> {
    let operation = *record.cigar.get(operation_index)?;
    let mut reference_position = record.start;
    let mut read_position = 0_usize;
    for preceding in &record.cigar[..operation_index] {
        match preceding.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                reference_position = reference_position.saturating_add(u64::from(preceding.len));
                read_position = read_position.saturating_add(preceding.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(preceding.len));
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(preceding.len as usize);
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }

    if let Some(operation_offset) = operation_offset {
        if !matches!(
            operation.kind,
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch
        ) || operation_offset >= operation.len
        {
            return None;
        }
        let genomic_position = reference_position.saturating_add(u64::from(operation_offset));
        let aligned_read_position = read_position.saturating_add(operation_offset as usize);
        let (observed, quality) = match &record.sequence {
            SequenceData::Present { bases, qualities } => (
                bases.get(aligned_read_position).copied(),
                qualities
                    .as_deref()
                    .and_then(|values| values.get(aligned_read_position))
                    .copied(),
            ),
            SequenceData::NotLoaded | SequenceData::Missing => (None, None),
        };
        let reference_base = reference.and_then(|slice| slice.base_at(genomic_position));
        let classification = classify_aligned_base(operation.kind, observed, reference_base);
        if matches!(
            classification,
            AlignedBaseClassification::Match | AlignedBaseClassification::Unknown
        ) {
            return None;
        }
        let base_label = |base: Option<u8>| {
            base.map_or_else(
                || "unavailable".to_owned(),
                |base| char::from(base.to_ascii_uppercase()).to_string(),
            )
        };
        let mut fields = vec![
            (
                "Selected event".to_owned(),
                if classification == AlignedBaseClassification::EncodingConflict {
                    "Alignment encoding conflict".to_owned()
                } else {
                    "Base substitution".to_owned()
                },
            ),
            ("Event length".to_owned(), "1 bp".to_owned()),
            (
                "Event locus".to_owned(),
                format!("{contig_name}:{}", genomic_position.saturating_add(1)),
            ),
            (
                "Aligned read position".to_owned(),
                aligned_read_position.saturating_add(1).to_string(),
            ),
            ("Reference base".to_owned(), base_label(reference_base)),
            ("Observed base".to_owned(), base_label(observed)),
            (
                "Base quality".to_owned(),
                quality.map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
            ),
        ];
        if classification == AlignedBaseClassification::EncodingConflict {
            let cigar_assertion = match operation.kind {
                CigarOpKind::SequenceMismatch => "X (mismatch)",
                CigarOpKind::SequenceMatch => "= (match)",
                _ => unreachable!("only explicit CIGAR assertions can conflict"),
            };
            let comparison = match observed.zip(reference_base) {
                Some((observed, reference)) if observed.eq_ignore_ascii_case(&reference) => {
                    format!(
                        "bases agree ({} = {})",
                        base_label(observed.into()),
                        base_label(reference.into())
                    )
                }
                Some((observed, reference)) => format!(
                    "bases differ ({} ≠ {})",
                    base_label(observed.into()),
                    base_label(reference.into())
                ),
                None => "unavailable".to_owned(),
            };
            fields.extend([
                ("CIGAR assertion".to_owned(), cigar_assertion.to_owned()),
                ("Observed comparison".to_owned(), comparison),
                (
                    "Validation".to_owned(),
                    match operation.kind {
                        CigarOpKind::SequenceMismatch => {
                            "CIGAR X asserts a mismatch, but the loaded read and reference bases agree"
                        }
                        CigarOpKind::SequenceMatch => {
                            "CIGAR = asserts a match, but the loaded read and reference bases differ"
                        }
                        _ => unreachable!(),
                    }
                    .to_owned(),
                ),
            ]);
        } else {
            fields.push((
                "Substitution evidence".to_owned(),
                match classification {
                    AlignedBaseClassification::DeclaredSubstitution => {
                        "explicit CIGAR X (unvalidated: a base is unavailable)"
                    }
                    AlignedBaseClassification::Substitution
                        if operation.kind == CigarOpKind::SequenceMismatch =>
                    {
                        "explicit CIGAR X, confirmed by read/reference comparison"
                    }
                    AlignedBaseClassification::Substitution => "read/reference comparison",
                    _ => unreachable!(),
                }
                .to_owned(),
            ));
        }
        return Some(ReadEventDetails {
            fields,
            copy_sequence: observed.map(|base| char::from(base).to_string()),
        });
    }

    let kind = match operation.kind {
        CigarOpKind::Insertion => "Insertion",
        CigarOpKind::Deletion => "Deletion",
        CigarOpKind::ReferenceSkip => "Reference skip",
        CigarOpKind::SoftClip => "Soft clip",
        CigarOpKind::HardClip => "Hard clip",
        CigarOpKind::Padding => "Padding",
        CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
            return None;
        }
    };
    let reference_label = match operation.kind {
        CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => format!(
            "{contig_name}:{}-{}",
            reference_position + 1,
            reference_position.saturating_add(u64::from(operation.len))
        ),
        _ => format!(
            "{contig_name}:{}|{}",
            reference_position,
            reference_position.saturating_add(1)
        ),
    };
    let mut fields = vec![
        ("Selected event".to_owned(), kind.to_owned()),
        ("Event length".to_owned(), format!("{} bp", operation.len)),
        ("Event locus".to_owned(), reference_label),
    ];
    let event_read_range = matches!(
        operation.kind,
        CigarOpKind::Insertion | CigarOpKind::SoftClip
    )
    .then(|| {
        let end = read_position.saturating_add(operation.len as usize);
        read_position..end
    });
    let mut copy_sequence = None;
    if let Some(range) = event_read_range {
        fields.push((
            "Read positions".to_owned(),
            format!("{}-{}", range.start + 1, range.end),
        ));
        match &record.sequence {
            SequenceData::Present { bases, qualities } => {
                let (sequence, event_qualities) =
                    aligned_event_sequence(bases, qualities.as_deref(), range);
                fields.push((
                    "Sequence preview".to_owned(),
                    bounded_sequence_preview(&sequence),
                ));
                if let Some(event_qualities) = event_qualities {
                    let mean = (!event_qualities.is_empty()).then(|| {
                        event_qualities
                            .iter()
                            .map(|value| u64::from(*value))
                            .sum::<u64>() as f64
                            / event_qualities.len() as f64
                    });
                    fields.push((
                        "Event qualities".to_owned(),
                        mean.map_or_else(
                            || "unavailable".to_owned(),
                            |mean| format!("mean {mean:.1}"),
                        ),
                    ));
                }
                let copy_length = sequence.len().min(EVENT_SEQUENCE_COPY_BASES);
                copy_sequence =
                    Some(String::from_utf8_lossy(&sequence[..copy_length]).into_owned());
                if sequence.len() > copy_length {
                    fields.push((
                        "Copy limit".to_owned(),
                        format!(
                            "first {copy_length} of {} bases; export is required for the complete event",
                            sequence.len()
                        ),
                    ));
                }
            }
            SequenceData::NotLoaded => {
                fields.push(("Sequence preview".to_owned(), "not loaded".to_owned()));
            }
            SequenceData::Missing => {
                fields.push(("Sequence preview".to_owned(), "missing".to_owned()));
            }
        }
    }
    Some(ReadEventDetails {
        fields,
        copy_sequence,
    })
}

pub(crate) fn bounded_sequence_preview(sequence: &[u8]) -> String {
    let preview_length = sequence.len().min(EVENT_SEQUENCE_PREVIEW_BASES);
    let mut preview = String::from_utf8_lossy(&sequence[..preview_length]).into_owned();
    if sequence.len() > preview_length {
        let remaining = sequence.len() - preview_length;
        let _ = write!(preview, "… (+{remaining} bases)");
    }
    preview
}

pub(crate) fn bounded_sequence_copy(sequence: &[u8]) -> BoundedSequenceCopy {
    let copied_bases = sequence.len().min(SEQUENCE_COPY_BASES);
    BoundedSequenceCopy {
        text: String::from_utf8_lossy(&sequence[..copied_bases]).into_owned(),
        copied_bases,
        total_bases: sequence.len(),
    }
}

pub(crate) fn bounded_oriented_sequence_copy(
    sequence: &[u8],
    reverse: bool,
) -> BoundedSequenceCopy {
    let copied_bases = sequence.len().min(SEQUENCE_COPY_BASES);
    if !reverse {
        return BoundedSequenceCopy {
            text: String::from_utf8_lossy(&sequence[..copied_bases]).into_owned(),
            copied_bases,
            total_bases: sequence.len(),
        };
    }
    let text = (0..copied_bases)
        .filter_map(|position| {
            sequence
                .get(sequence.len().checked_sub(position.saturating_add(1))?)
                .copied()
                .map(complement_base)
                .map(char::from)
        })
        .collect();
    BoundedSequenceCopy {
        text,
        copied_bases,
        total_bases: sequence.len(),
    }
}

pub(crate) fn bounded_aligned_sequence_copy(sequence: &[u8]) -> BoundedSequenceCopy {
    let copied_bases = sequence.len().min(SEQUENCE_COPY_BASES);
    BoundedSequenceCopy {
        text: String::from_utf8_lossy(&sequence[..copied_bases]).into_owned(),
        copied_bases,
        total_bases: sequence.len(),
    }
}

pub(crate) fn sequence_copy_hover(sequence: Option<&BoundedSequenceCopy>, kind: &str) -> String {
    sequence.map_or_else(String::new, |sequence| {
        if sequence.copied_bases == sequence.total_bases {
            format!("Copy all {} {kind} bases", sequence.total_bases)
        } else {
            format!(
                "Copy the first {} of {} {kind} bases; use export for the complete sequence",
                sequence.copied_bases, sequence.total_bases
            )
        }
    })
}

pub(crate) fn inspector_fields_text(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(label, value)| format!("{label}: {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn inspector_field_value<'a>(snapshot: &'a InspectorSnapshot, label: &str) -> Option<&'a str> {
    snapshot
        .fields
        .iter()
        .find(|(candidate, _)| candidate == label)
        .map(|(_, value)| value.as_str())
}

fn sequence_field(
    id: &str,
    label: &str,
    sequence: &str,
    total_bases: usize,
    copied_bases: usize,
) -> InspectorField {
    const PREVIEW_BASES: usize = 160;
    let mut display = sequence.chars().take(PREVIEW_BASES).collect::<String>();
    if sequence.chars().count() > PREVIEW_BASES {
        display.push('…');
    }
    InspectorField::new(
        id,
        label,
        InspectorValue::Sequence {
            display,
            total_bases,
            truncated: copied_bases < total_bases,
        },
    )
    .with_copy_value(sequence)
    .with_provenance(if copied_bases < total_bases {
        InspectorProvenance::Truncated {
            displayed: copied_bases,
            total: Some(total_bases),
            reason: "sequence clipboard resource limit".to_owned(),
        }
    } else {
        InspectorProvenance::Source
    })
}

/// Builds the complete read/read-event presentation document. Runtime
/// availability is supplied explicitly so the adapter, rather than the UI,
/// owns which actions exist and why any of them are disabled.
#[allow(clippy::too_many_lines)]
pub(crate) fn alignment_inspector_document(
    inspector: &InspectorSnapshot,
    identity: impl Into<String>,
    visible_reference: Option<&BoundedSequenceCopy>,
    mate_target_available: bool,
    available_panel_slots: usize,
) -> InspectorDocument {
    let title = inspector_field_value(inspector, "Read").unwrap_or("Alignment segment");
    let mut document = inspector.document(identity, title);
    document.header.subtitle = inspector_field_value(inspector, "Alignment segment")
        .map(|role| format!("{role} alignment segment"));
    document.header.source = inspector_field_value(inspector, "Track").map(str::to_owned);
    document.header.badges.push(InspectorBadge {
        label: if inspector.event_sequence.is_some() {
            "Read event".to_owned()
        } else {
            "Read".to_owned()
        },
        tone: InspectorTone::Informative,
    });
    if let Some(summary) = document.sections.first_mut() {
        summary.id = "alignment-summary".to_owned();
        summary.title = "Alignment evidence".to_owned();
        summary.fields.retain(|field| {
            !matches!(
                field.label.as_str(),
                "Track" | "Read" | "Alignment segment" | "Position"
            ) && !matches!(
                field.value,
                InspectorValue::Missing {
                    consequential: false,
                    ..
                }
            )
        });
    }
    if let Some(cigar) = &inspector.cigar {
        if let Some(cigar_field) = document.sections.first_mut().and_then(|summary| {
            summary
                .fields
                .iter_mut()
                .find(|field| field.label == "CIGAR")
        }) {
            let display = cigar_field.value.display_text();
            cigar_field.id = "cigar".to_owned();
            cigar_field.value = InspectorValue::Identifier(display.clone());
            cigar_field.copy_value = Some(cigar.clone());
            if display != *cigar {
                cigar_field.provenance = InspectorProvenance::Truncated {
                    displayed: display.len(),
                    total: Some(cigar.len()),
                    reason: "CIGAR presentation limit".to_owned(),
                };
            }
        } else {
            document.sections.push(
                InspectorSection::new("exact-cigar", "Exact CIGAR", 90)
                    .collapsed()
                    .with_fields(vec![
                        InspectorField::new(
                            "cigar",
                            "CIGAR",
                            InspectorValue::Identifier(cigar.clone()),
                        )
                        .with_copy_value(cigar),
                    ]),
            );
        }
    }
    let mut sequences = Vec::new();
    if let Some(sequence) = &inspector.read_sequence {
        sequences.push(sequence_field(
            "read-sequence",
            "Read sequence",
            &sequence.text,
            sequence.total_bases,
            sequence.copied_bases,
        ));
    }
    if let Some(sequence) = &inspector.event_sequence {
        sequences.push(sequence_field(
            "event-sequence",
            "Event sequence",
            sequence,
            sequence.len(),
            sequence.len(),
        ));
    }
    if !sequences.is_empty() {
        document.sections.push(
            InspectorSection::new("sequences", "Sequences", 100)
                .collapsed()
                .with_fields(sequences),
        );
    }

    document
        .actions
        .push(InspectorAction::new(InspectorActionId::CopyDetails, "Copy details").primary());
    if inspector.cigar.is_some() {
        document.actions.push(InspectorAction::new(
            InspectorActionId::CopyCigar,
            "Copy CIGAR",
        ));
    }
    let copy_read = InspectorAction::new(InspectorActionId::CopyReadSequence, "Copy read sequence")
        .with_hover_text(sequence_copy_hover(
            inspector.read_sequence.as_ref(),
            "read",
        ));
    document.actions.push(if inspector.read_sequence.is_some() {
        copy_read
    } else {
        copy_read.disabled("Sequence was not loaded or is missing from the record")
    });
    let copy_tags =
        InspectorAction::new(InspectorActionId::CopyAuxiliaryTags, "Copy auxiliary tags");
    document.actions.push(if inspector.auxiliary.is_empty() {
        copy_tags.disabled("This record has no loaded auxiliary tags")
    } else {
        copy_tags
    });
    if inspector.event_sequence.is_some() {
        document.actions.push(InspectorAction::new(
            InspectorActionId::CopyEventSequence,
            "Copy event sequence",
        ));
    }
    let copy_reference = InspectorAction::new(
        InspectorActionId::CopyVisibleReference,
        "Copy visible reference",
    )
    .with_hover_text(sequence_copy_hover(visible_reference, "reference"));
    document.actions.push(if visible_reference.is_some() {
        copy_reference
    } else {
        copy_reference.disabled("Zoom to base detail with a reference FASTA available")
    });
    let open_mate = InspectorAction::new(InspectorActionId::OpenMate, "Open mate in new panel");
    document
        .actions
        .push(if mate_target_available && available_panel_slots > 0 {
            open_mate
        } else {
            open_mate.disabled("A mapped mate and space for another panel are required")
        });
    document.actions.push(InspectorAction::new(
        InspectorActionId::MoveReadToTop,
        "Move selected to top",
    ));
    document.actions.push(
        InspectorAction::new(InspectorActionId::ClearSelection, "Clear selection").low_emphasis(),
    );
    document
}

pub(crate) fn pileup_inspector_document(
    inspector: &InspectorSnapshot,
    visible_consensus: Option<&BoundedSequenceCopy>,
    visible_reference: Option<&BoundedSequenceCopy>,
) -> InspectorDocument {
    let mut document = inspector.document("pileup", "Pileup");
    document.header.source = inspector_field_value(inspector, "Track").map(str::to_owned);
    document.header.badges.push(InspectorBadge {
        label: "Pileup".to_owned(),
        tone: InspectorTone::Informative,
    });
    document
        .actions
        .push(InspectorAction::new(InspectorActionId::CopyDetails, "Copy details").primary());
    let consensus = InspectorAction::new(
        InspectorActionId::CopyVisibleConsensus,
        "Copy visible consensus",
    )
    .with_hover_text(sequence_copy_hover(visible_consensus, "consensus"));
    document.actions.push(if visible_consensus.is_some() {
        consensus
    } else {
        consensus.disabled("Zoom to exact base resolution to calculate consensus")
    });
    let reference = InspectorAction::new(
        InspectorActionId::CopyVisibleReference,
        "Copy visible reference",
    )
    .with_hover_text(sequence_copy_hover(visible_reference, "reference"));
    document.actions.push(if visible_reference.is_some() {
        reference
    } else {
        reference.disabled("Zoom to base detail with a reference FASTA available")
    });
    document.actions.push(
        InspectorAction::new(InspectorActionId::ClearSelection, "Clear selection").low_emphasis(),
    );
    document
}

pub(crate) fn modification_inspector_document(inspector: &InspectorSnapshot) -> InspectorDocument {
    let mut document = inspector.document("modification-frequency", "Modification frequency");
    document.header.source = inspector_field_value(inspector, "Track").map(str::to_owned);
    document.header.badges.push(InspectorBadge {
        label: "Modified bases".to_owned(),
        tone: InspectorTone::Informative,
    });
    document.actions.extend([
        InspectorAction::new(InspectorActionId::CopyDetails, "Copy details").primary(),
        InspectorAction::new(InspectorActionId::ClearSelection, "Clear selection").low_emphasis(),
    ]);
    document
}

pub(crate) fn junction_inspector_document(inspector: &InspectorSnapshot) -> InspectorDocument {
    let mut document = inspector.document("splice-junction", "Splice junction");
    document.header.source = inspector_field_value(inspector, "Track").map(str::to_owned);
    document.header.badges.push(InspectorBadge {
        label: "Junction".to_owned(),
        tone: InspectorTone::Informative,
    });
    document.actions.extend([
        InspectorAction::new(InspectorActionId::CopyDetails, "Copy details").primary(),
        InspectorAction::new(InspectorActionId::ExportJunctionBed, "Export junction BED…"),
        InspectorAction::new(InspectorActionId::ExportSashimiPng, "Export Sashimi PNG…"),
        InspectorAction::new(InspectorActionId::ExportSashimiSvg, "Export Sashimi SVG…"),
        InspectorAction::new(InspectorActionId::ClearSelection, "Clear selection").low_emphasis(),
    ]);
    document
}

pub(crate) const MAX_FEATURE_INSPECTOR_BLOCKS: usize = 128;
pub(crate) const MAX_FEATURE_INSPECTOR_ATTRIBUTES: usize = 256;
pub(crate) const MAX_FEATURE_INSPECTOR_VALUE_BYTES: usize = 4_096;

#[allow(clippy::too_many_lines)]
pub(crate) fn auxiliary_tags_text(tags: &[(String, String)]) -> String {
    let mut text = String::new();
    let mut omitted = 0;
    for (index, (tag, value)) in tags.iter().enumerate() {
        let separator = usize::from(index > 0);
        let required = separator
            .saturating_add(tag.len())
            .saturating_add(1)
            .saturating_add(value.len());
        if text.len().saturating_add(required) > AUXILIARY_TAGS_COPY_BYTES {
            omitted = tags.len() - index;
            break;
        }
        if index > 0 {
            text.push('\n');
        }
        let _ = write!(text, "{tag}:{value}");
    }
    if omitted > 0 {
        let _ = write!(text, "\n… {omitted} tags omitted at the clipboard limit");
    }
    text
}

pub(crate) fn aligned_event_sequence(
    bases: &[u8],
    qualities: Option<&[u8]>,
    range: std::ops::Range<usize>,
) -> (Vec<u8>, Option<Vec<u8>>) {
    let mut event_bases = Vec::with_capacity(range.len());
    let mut event_qualities = qualities.map(|_| Vec::with_capacity(range.len()));
    for read_position in range {
        let Some(base) = bases.get(read_position).copied() else {
            continue;
        };
        event_bases.push(base);
        if let (Some(source), Some(output)) = (qualities, &mut event_qualities)
            && let Some(quality) = source.get(read_position)
        {
            output.push(*quality);
        }
    }
    (event_bases, event_qualities)
}

pub(crate) const fn complement_base(base: u8) -> u8 {
    match base.to_ascii_uppercase() {
        b'A' => b'T',
        b'C' => b'G',
        b'G' => b'C',
        b'T' | b'U' => b'A',
        other => other,
    }
}

/// The read inspector's expensive derived strings for one record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReadInspectorDerived {
    pub(crate) modified_bases: String,
    pub(crate) cigar: String,
}

/// Remembers [`ReadInspectorDerived`] for the selected record (UI-4). The inspector is rebuilt
/// every frame; for a long read, decoding its modified bases and formatting its CIGAR each time
/// dominated the frame. The key is the record itself, so the cache can never serve another
/// read's strings: comparing two records is a linear scan with no allocation, far cheaper than
/// either derivation, and a changed selection or refreshed record simply misses.
#[derive(Debug, Default)]
pub(crate) struct ReadInspectorDerivedCache {
    entry: Option<(rgv_hts::AlignmentRecord, ReadInspectorDerived)>,
}

impl ReadInspectorDerivedCache {
    pub(crate) fn get_or_compute(
        &mut self,
        record: &rgv_hts::AlignmentRecord,
        compute: impl FnOnce(&rgv_hts::AlignmentRecord) -> ReadInspectorDerived,
    ) -> ReadInspectorDerived {
        if let Some((cached, derived)) = &self.entry
            && cached == record
        {
            return derived.clone();
        }
        let derived = compute(record);
        self.entry = Some((record.clone(), derived.clone()));
        derived
    }
}

pub(crate) fn format_cigar(cigar: &[rgv_hts::CigarOp]) -> String {
    if cigar.is_empty() {
        return "*".to_owned();
    }
    cigar.iter().fold(String::new(), |mut output, operation| {
        let code = match operation.kind {
            CigarOpKind::Match => 'M',
            CigarOpKind::Insertion => 'I',
            CigarOpKind::Deletion => 'D',
            CigarOpKind::ReferenceSkip => 'N',
            CigarOpKind::SoftClip => 'S',
            CigarOpKind::HardClip => 'H',
            CigarOpKind::Padding => 'P',
            CigarOpKind::SequenceMatch => '=',
            CigarOpKind::SequenceMismatch => 'X',
        };
        write!(output, "{}{code}", operation.len).expect("writing to a String cannot fail");
        output
    })
}

pub(crate) fn inspector_cigar_value(cigar: &[rgv_hts::CigarOp], formatted: &str) -> String {
    const INLINE_OPERATION_LIMIT: usize = 32;
    const INLINE_BYTE_LIMIT: usize = 256;
    const PREVIEW_BYTES: usize = 72;

    if cigar.len() <= INLINE_OPERATION_LIMIT && formatted.len() <= INLINE_BYTE_LIMIT {
        return formatted.to_owned();
    }
    let query_span = cigar.iter().fold(0_u64, |total, operation| {
        if matches!(
            operation.kind,
            CigarOpKind::Match
                | CigarOpKind::Insertion
                | CigarOpKind::SoftClip
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
        ) {
            total.saturating_add(u64::from(operation.len))
        } else {
            total
        }
    });
    let reference_span = cigar.iter().fold(0_u64, |total, operation| {
        if matches!(
            operation.kind,
            CigarOpKind::Match
                | CigarOpKind::Deletion
                | CigarOpKind::ReferenceSkip
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
        ) {
            total.saturating_add(u64::from(operation.len))
        } else {
            total
        }
    });
    let prefix = &formatted[..formatted.len().min(PREVIEW_BYTES)];
    let suffix_start = formatted.len().saturating_sub(PREVIEW_BYTES);
    let suffix = &formatted[suffix_start..];
    format!(
        "{} operations · {query_span} bp query span · {reference_span} bp reference span · {prefix}…{suffix}",
        cigar.len()
    )
}

pub(crate) fn format_aux_value(value: &AuxValue) -> String {
    match value {
        AuxValue::Character(value) => char::from(*value).to_string(),
        AuxValue::Signed(value) => value.to_string(),
        AuxValue::Unsigned(value) => value.to_string(),
        AuxValue::Float(value) => value.to_string(),
        AuxValue::Double(value) => value.to_string(),
        AuxValue::String(value) | AuxValue::HexByteArray(value) => bounded_auxiliary_value(value),
        AuxValue::SignedArray(values) => format_array(values),
        AuxValue::UnsignedArray(values) => format_array(values),
        AuxValue::FloatArray(values) => format_array(values),
    }
}

pub(crate) fn bounded_auxiliary_value(value: &str) -> String {
    if value.len() <= AUXILIARY_VALUE_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = AUXILIARY_VALUE_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}… (+{} bytes)",
        &value[..end],
        value.len().saturating_sub(end)
    )
}

pub(crate) fn truncate_string_to_bytes(value: &mut String, maximum: usize) {
    if value.len() <= maximum {
        return;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

pub(crate) fn bounded_preview(value: &str, maximum_characters: usize) -> Cow<'_, str> {
    if value.chars().count() <= maximum_characters {
        return Cow::Borrowed(value);
    }
    let mut preview = value.chars().take(maximum_characters).collect::<String>();
    preview.push('…');
    Cow::Owned(preview)
}

pub(crate) fn format_array<T: std::fmt::Display>(values: &[T]) -> String {
    const SHOWN: usize = 16;
    let mut formatted = values
        .iter()
        .take(SHOWN)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if values.len() > SHOWN {
        write!(formatted, ",… ({} values)", values.len()).expect("writing to a String cannot fail");
    }
    format!("[{formatted}]")
}
