use super::super::*;

pub(crate) struct ReadEventDetails {
    pub(crate) fields: Vec<(String, String)>,
    pub(crate) copy_sequence: Option<String>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn read_event_details(
    record: &AlignmentRecord,
    operation_index: usize,
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
                let (sequence, event_qualities) = oriented_event_sequence(
                    bases,
                    qualities.as_deref(),
                    range,
                    record.flags.is_reverse(),
                );
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

pub(crate) fn oriented_event_sequence(
    bases: &[u8],
    qualities: Option<&[u8]>,
    range: std::ops::Range<usize>,
    reverse: bool,
) -> (Vec<u8>, Option<Vec<u8>>) {
    let mut event_bases = Vec::with_capacity(range.len());
    let mut event_qualities = qualities.map(|_| Vec::with_capacity(range.len()));
    for read_position in range {
        let index = if reverse {
            bases.len().checked_sub(read_position.saturating_add(1))
        } else {
            Some(read_position)
        };
        let Some(index) = index else {
            continue;
        };
        let Some(base) = bases.get(index).copied() else {
            continue;
        };
        event_bases.push(if reverse { complement_base(base) } else { base });
        if let (Some(source), Some(output)) = (qualities, &mut event_qualities)
            && let Some(quality) = source.get(index)
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

pub(crate) fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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
