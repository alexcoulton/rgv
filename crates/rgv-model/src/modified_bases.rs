use thiserror::Error;

use crate::{AlignmentFlags, AlignmentRecord, AuxField, AuxValue, CigarOpKind, SequenceData};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BaseModificationStrand {
    Forward,
    Reverse,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ImplicitModificationMode {
    Canonical,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ModificationCode {
    Letter(u8),
    Chebi(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModifiedBaseCall {
    pub canonical_base: u8,
    pub strand: BaseModificationStrand,
    pub code: ModificationCode,
    pub implicit_mode: ImplicitModificationMode,
    /// Zero-based position in the original sequence reported by the sequencer.
    ///
    /// Unlike `SEQ`, SAM `MM`/`ML` coordinates are not reoriented when flag
    /// `0x10` is set. Use [`aligned_position_for_sequencer_position`] before
    /// mapping this position through a CIGAR.
    pub sequencer_position: usize,
    pub probability: Option<u8>,
}

/// Decode SAM `MM`/`ML` tags in their original sequencer orientation.
pub fn decode_modified_bases(
    record: &AlignmentRecord,
) -> Result<Vec<ModifiedBaseCall>, ModifiedBaseError> {
    let Some(mm) = find_tag(&record.auxiliary_fields, [b'M', b'M'])
        .or_else(|| find_tag(&record.auxiliary_fields, [b'M', b'm']))
    else {
        return Ok(Vec::new());
    };
    let ml = find_tag(&record.auxiliary_fields, [b'M', b'L'])
        .or_else(|| find_tag(&record.auxiliary_fields, [b'M', b'l']))
        .map(|field| &field.value);
    let sequence = match &record.sequence {
        SequenceData::Present { bases, .. } => bases,
        SequenceData::Missing => return Err(ModifiedBaseError::MissingSequence),
        SequenceData::NotLoaded => return Err(ModifiedBaseError::SequenceNotLoaded),
    };
    let hard_clipped = record
        .cigar
        .iter()
        .any(|operation| operation.kind == CigarOpKind::HardClip);
    let mn = find_tag(&record.auxiliary_fields, [b'M', b'N']).map(|field| &field.value);
    decode_modified_bases_parts_checked(sequence, record.flags, &mm.value, ml, mn, hard_clipped)
}

/// Whether `MM` coordinates can be resolved against the stored `SEQ`.
///
/// `MM` counts bases of the sequence as it was when the tag was written. `MN` records that
/// length: when present it must equal the stored sequence length. Without `MN`, a hard-clipped
/// record is unresolvable because aligners commonly copy `MM` unchanged onto hard-clipped
/// supplementary alignments, where the same deltas would land on the wrong bases.
pub fn modified_bases_resolvable(
    sequence_length: usize,
    mn: Option<&AuxValue>,
    hard_clipped: bool,
) -> bool {
    match mn {
        Some(AuxValue::Unsigned(length)) => u64::try_from(sequence_length) == Ok(*length),
        Some(AuxValue::Signed(length)) => i64::try_from(sequence_length) == Ok(*length),
        Some(_) => false,
        None => !hard_clipped,
    }
}

/// Like [`decode_modified_bases_parts`], but reports no calls (rather than misplaced calls or an
/// error) for a record whose `MM` tag cannot be resolved against the stored sequence; see
/// [`modified_bases_resolvable`].
pub fn decode_modified_bases_parts_checked(
    sequence: &[u8],
    flags: AlignmentFlags,
    mm: &AuxValue,
    ml: Option<&AuxValue>,
    mn: Option<&AuxValue>,
    hard_clipped: bool,
) -> Result<Vec<ModifiedBaseCall>, ModifiedBaseError> {
    if !modified_bases_resolvable(sequence.len(), mn, hard_clipped) {
        return Ok(Vec::new());
    }
    decode_modified_bases_parts(sequence, flags, mm, ml)
}

/// Decode modified bases from packed-record components without materializing an alignment.
pub fn decode_modified_bases_parts(
    sequence: &[u8],
    flags: AlignmentFlags,
    mm: &AuxValue,
    ml: Option<&AuxValue>,
) -> Result<Vec<ModifiedBaseCall>, ModifiedBaseError> {
    let AuxValue::String(mm) = mm else {
        return Err(ModifiedBaseError::InvalidMmType);
    };
    let probabilities = ml.map(probability_values).transpose()?.unwrap_or_default();

    let mut calls = Vec::new();
    let mut probability_index = 0;
    for raw_group in mm.split(';').filter(|group| !group.is_empty()) {
        // A group without a delta list (`C+m?;`) is valid SAM: the modification was assessed
        // and no base of this read carries it.
        let (header, raw_deltas) = raw_group
            .split_once(',')
            .map_or((raw_group, None), |(header, deltas)| (header, Some(deltas)));
        let group = parse_group_header(header)?;
        let deltas = raw_deltas
            .into_iter()
            .flat_map(|deltas| deltas.split(','))
            .map(|delta| {
                delta
                    .parse::<usize>()
                    .map_err(|_| ModifiedBaseError::InvalidDelta(delta.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let positions = locate_calls(sequence, flags, group.canonical_base, &deltas)?;
        for position in positions {
            for code in &group.codes {
                let probability = probabilities.get(probability_index).copied();
                probability_index += usize::from(!probabilities.is_empty());
                calls.push(ModifiedBaseCall {
                    canonical_base: group.canonical_base,
                    strand: group.strand,
                    code: *code,
                    implicit_mode: group.implicit_mode,
                    sequencer_position: position,
                    probability,
                });
            }
        }
    }
    if !probabilities.is_empty() && probability_index != probabilities.len() {
        return Err(ModifiedBaseError::ProbabilityCountMismatch {
            calls: probability_index,
            probabilities: probabilities.len(),
        });
    }
    Ok(calls)
}

/// The modification codes an `MM` tag declares in its group headers, in tag order and without
/// repeats, without locating any call. Malformed groups are skipped. A cheap way to learn which
/// modifications a record was assessed for (a legend, a code list) without decoding it.
#[must_use]
pub fn modification_codes_in_tag(mm: &AuxValue) -> Vec<ModificationCode> {
    let AuxValue::String(mm) = mm else {
        return Vec::new();
    };
    let mut codes = Vec::new();
    for group in mm.split(';').filter(|group| !group.is_empty()) {
        let header = group.split_once(',').map_or(group, |(header, _)| header);
        if let Ok(group) = parse_group_header(header) {
            for code in group.codes {
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
        }
    }
    codes
}

struct ModificationGroup {
    canonical_base: u8,
    strand: BaseModificationStrand,
    codes: Vec<ModificationCode>,
    implicit_mode: ImplicitModificationMode,
}

fn parse_group_header(header: &str) -> Result<ModificationGroup, ModifiedBaseError> {
    let bytes = header.as_bytes();
    if bytes.len() < 3
        || !matches!(
            bytes[0].to_ascii_uppercase(),
            b'A' | b'C' | b'G' | b'T' | b'U' | b'N'
        )
    {
        return Err(ModifiedBaseError::MalformedGroup(header.to_owned()));
    }
    let strand = match bytes[1] {
        b'+' => BaseModificationStrand::Forward,
        b'-' => BaseModificationStrand::Reverse,
        _ => return Err(ModifiedBaseError::InvalidStrand(bytes[1])),
    };
    let (code_bytes, implicit_mode) = match bytes.last() {
        Some(b'?') => (
            &bytes[2..bytes.len() - 1],
            ImplicitModificationMode::Unknown,
        ),
        Some(b'.') => (
            &bytes[2..bytes.len() - 1],
            ImplicitModificationMode::Canonical,
        ),
        _ => (&bytes[2..], ImplicitModificationMode::Canonical),
    };
    if code_bytes.is_empty() {
        return Err(ModifiedBaseError::MissingModificationCode);
    }
    let codes = if code_bytes.iter().all(u8::is_ascii_digit) {
        let code = std::str::from_utf8(code_bytes)
            .map_err(|_| ModifiedBaseError::InvalidModificationCode)?
            .parse()
            .map_err(|_| ModifiedBaseError::InvalidModificationCode)?;
        vec![ModificationCode::Chebi(code)]
    } else if code_bytes.iter().all(u8::is_ascii_alphabetic) {
        code_bytes
            .iter()
            .copied()
            .map(ModificationCode::Letter)
            .collect()
    } else {
        return Err(ModifiedBaseError::InvalidModificationCode);
    };
    Ok(ModificationGroup {
        canonical_base: bytes[0].to_ascii_uppercase(),
        strand,
        codes,
        implicit_mode,
    })
}

fn locate_calls(
    sequence: &[u8],
    flags: AlignmentFlags,
    canonical_base: u8,
    deltas: &[usize],
) -> Result<Vec<usize>, ModifiedBaseError> {
    let mut result = Vec::with_capacity(deltas.len());
    let mut cursor = 0;
    for delta in deltas {
        let mut remaining = *delta;
        let position = loop {
            if cursor >= sequence.len() {
                return Err(ModifiedBaseError::DeltaBeyondSequence);
            }
            let position = cursor;
            cursor += 1;
            if !sequencer_base_matches(sequence, flags, position, canonical_base) {
                continue;
            }
            if remaining == 0 {
                break position;
            }
            remaining -= 1;
        };
        result.push(position);
    }
    Ok(result)
}

/// Convert a coordinate in the original sequencer orientation to the position
/// used by the aligned `SEQ`, `QUAL`, and CIGAR fields.
pub fn aligned_position_for_sequencer_position(
    sequence_length: usize,
    flags: AlignmentFlags,
    sequencer_position: usize,
) -> Option<usize> {
    if sequencer_position >= sequence_length {
        return None;
    }
    if flags.is_reverse() {
        sequence_length.checked_sub(sequencer_position.checked_add(1)?)
    } else {
        Some(sequencer_position)
    }
}

/// Whether an aligned SAM/BAM `SEQ` base represents the requested canonical
/// base at an original-sequencer coordinate used by an `MM` tag.
///
/// The `N` canonical code matches every sequenced base, as required by the SAM
/// optional-fields specification.
pub fn sequencer_base_matches(
    sequence: &[u8],
    flags: AlignmentFlags,
    sequencer_position: usize,
    canonical_base: u8,
) -> bool {
    let Some(aligned_position) =
        aligned_position_for_sequencer_position(sequence.len(), flags, sequencer_position)
    else {
        return false;
    };
    let Some(aligned_base) = sequence.get(aligned_position).copied() else {
        return false;
    };
    let canonical_base = canonical_base.to_ascii_uppercase();
    if canonical_base == b'N' {
        return true;
    }
    let sequencer_base = if flags.is_reverse() {
        complement(aligned_base)
    } else {
        aligned_base.to_ascii_uppercase()
    };
    sequencer_base == canonical_base
        || (canonical_base == b'U' && sequencer_base == b'T')
        || (canonical_base == b'T' && sequencer_base == b'U')
}

const fn complement(base: u8) -> u8 {
    match base.to_ascii_uppercase() {
        b'A' => b'T',
        b'C' => b'G',
        b'G' => b'C',
        b'T' | b'U' => b'A',
        other => other,
    }
}

fn find_tag(fields: &[AuxField], tag: [u8; 2]) -> Option<&AuxField> {
    fields.iter().find(|field| field.tag == tag)
}

fn probability_values(value: &AuxValue) -> Result<Vec<u8>, ModifiedBaseError> {
    match value {
        AuxValue::UnsignedArray(values) => values
            .iter()
            .map(|value| {
                u8::try_from(*value).map_err(|_| ModifiedBaseError::InvalidProbability(*value))
            })
            .collect(),
        _ => Err(ModifiedBaseError::InvalidMlType),
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ModifiedBaseError {
    #[error("MM tag must be a string")]
    InvalidMmType,
    #[error("ML tag must be an unsigned byte array")]
    InvalidMlType,
    #[error("alignment sequence is missing")]
    MissingSequence,
    #[error("alignment sequence was not loaded for this level of detail")]
    SequenceNotLoaded,
    #[error("malformed MM group: {0}")]
    MalformedGroup(String),
    #[error("invalid MM strand marker {0:?}")]
    InvalidStrand(u8),
    #[error("MM group has no modification code")]
    MissingModificationCode,
    #[error("invalid MM modification code")]
    InvalidModificationCode,
    #[error("invalid MM delta: {0}")]
    InvalidDelta(String),
    #[error("MM deltas extend beyond the stored read sequence")]
    DeltaBeyondSequence,
    #[error("ML probability {0} is outside the byte range")]
    InvalidProbability(u64),
    #[error("MM produced {calls} calls but ML contained {probabilities} probabilities")]
    ProbabilityCountMismatch { calls: usize, probabilities: usize },
}

#[cfg(test)]
mod tests {
    use rgv_core::ContigId;

    use super::*;
    use crate::{AlignmentFlags, CigarOp};

    fn record(mm: &str, probabilities: Vec<u64>) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 0,
            end: 10,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"modified".to_vec(),
            cigar: Vec::<CigarOp>::new(),
            sequence: SequenceData::Present {
                bases: b"ACCCGCCCTA".to_vec(),
                qualities: None,
            },
            auxiliary_fields: vec![
                AuxField {
                    tag: *b"MM",
                    value: AuxValue::String(mm.to_owned()),
                },
                AuxField {
                    tag: *b"ML",
                    value: AuxValue::UnsignedArray(probabilities),
                },
            ],
        }
    }

    #[test]
    fn decodes_delta_positions_and_probabilities() {
        let calls = decode_modified_bases(&record("C+m,0,2;", vec![240, 128])).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].sequencer_position, 1);
        assert_eq!(calls[1].sequencer_position, 5);
        assert_eq!(calls[0].probability, Some(240));
        assert_eq!(calls[1].probability, Some(128));
    }

    #[test]
    fn expands_multi_code_groups_in_ml_order() {
        let calls = decode_modified_bases(&record("C+mh?,1;", vec![10, 20])).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].sequencer_position, 2);
        assert_eq!(calls[0].code, ModificationCode::Letter(b'm'));
        assert_eq!(calls[1].code, ModificationCode::Letter(b'h'));
        assert_eq!(calls[1].implicit_mode, ImplicitModificationMode::Unknown);
        assert_eq!(calls[1].probability, Some(20));
    }

    #[test]
    fn rejects_ml_length_mismatch() {
        let error = decode_modified_bases(&record("C+m,0,1;", vec![1])).unwrap_err();
        assert!(matches!(
            error,
            ModifiedBaseError::ProbabilityCountMismatch { .. }
        ));
    }

    #[test]
    fn reverse_alignment_decodes_mm_against_original_sequencer_orientation() {
        let mut reverse = record("C+m,0;", vec![255]);
        reverse.flags = AlignmentFlags::from_bits(0x10);
        // Original ACCG, stored as reference-facing reverse complement CGGT.
        reverse.sequence = SequenceData::Present {
            bases: b"CGGT".to_vec(),
            qualities: None,
        };
        let calls = decode_modified_bases(&reverse).unwrap();
        assert_eq!(calls[0].sequencer_position, 1);
        assert_eq!(
            aligned_position_for_sequencer_position(4, reverse.flags, 1),
            Some(2)
        );
    }

    #[test]
    fn empty_group_contributes_no_calls_and_keeps_the_other_groups() {
        // Sequence ACCCGCCCTA: cytosines at 1, 2, 3, 5, 6, 7. `C+h?;` lists no positions;
        // `C+m,1;` skips one C and calls the second (position 2). The one ML value is its.
        let calls = decode_modified_bases(&record("C+h?;C+m,1;", vec![77])).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].code, ModificationCode::Letter(b'm'));
        assert_eq!(calls[0].sequencer_position, 2);
        assert_eq!(calls[0].probability, Some(77));

        assert_eq!(decode_modified_bases(&record("C+m?;", vec![])).unwrap(), []);
        assert_eq!(decode_modified_bases(&record("C+m?", vec![])).unwrap(), []);
        // A present-but-empty delta list is still malformed.
        assert!(matches!(
            decode_modified_bases(&record("C+m,;", vec![])).unwrap_err(),
            ModifiedBaseError::InvalidDelta(_)
        ));
        assert!(decode_modified_bases(&record("not-valid", vec![])).is_err());
    }

    fn with_mn(mut record: AlignmentRecord, length: u64) -> AlignmentRecord {
        record.auxiliary_fields.push(AuxField {
            tag: *b"MN",
            value: AuxValue::Unsigned(length),
        });
        record
    }

    #[test]
    fn mn_must_match_the_stored_sequence_length() {
        // The fixture sequence is 10 bases long.
        let matching = with_mn(record("C+m,0,2;", vec![240, 128]), 10);
        let calls = decode_modified_bases(&matching).unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|call| call.sequencer_position)
                .collect::<Vec<_>>(),
            [1, 5]
        );
        // MM was written for a 14-base read: the same deltas would hit the wrong bases, so the
        // record is skipped rather than decoded or reported as malformed.
        let stale = with_mn(record("C+m,0,2;", vec![240, 128]), 14);
        assert_eq!(decode_modified_bases(&stale).unwrap(), []);
    }

    #[test]
    fn hard_clipped_records_are_skipped_unless_mn_confirms_the_sequence() {
        let hard_clipped = |mut record: AlignmentRecord| {
            record.cigar.insert(
                0,
                CigarOp {
                    kind: crate::CigarOpKind::HardClip,
                    len: 4,
                },
            );
            record
        };
        // Even a tag that would fail with `DeltaBeyondSequence` is skipped, not an error.
        let unresolvable = hard_clipped(record("C+m,0,2,5;", vec![1, 2, 3]));
        assert_eq!(decode_modified_bases(&unresolvable).unwrap(), []);
        let confirmed = with_mn(hard_clipped(record("C+m,0,2;", vec![240, 128])), 10);
        assert_eq!(decode_modified_bases(&confirmed).unwrap().len(), 2);
        assert!(modified_bases_resolvable(10, None, false));
        assert!(!modified_bases_resolvable(10, None, true));
        assert!(!modified_bases_resolvable(
            10,
            Some(&AuxValue::String("10".to_owned())),
            false
        ));
    }

    #[test]
    fn canonical_n_counts_every_sequencer_base() {
        let calls = decode_modified_bases(&record("N+n,2;", vec![200])).unwrap();
        assert_eq!(calls[0].sequencer_position, 2);
    }

    #[test]
    fn tag_codes_are_read_from_the_group_headers_alone() {
        let mm = AuxValue::String("C+hm?,0,1;A+a.,3;C+m?;C+76792,0;bogus;".to_owned());
        assert_eq!(
            modification_codes_in_tag(&mm),
            vec![
                ModificationCode::Letter(b'h'),
                ModificationCode::Letter(b'm'),
                ModificationCode::Letter(b'a'),
                ModificationCode::Chebi(76_792),
            ]
        );
        assert!(modification_codes_in_tag(&AuxValue::Unsigned(1)).is_empty());
    }
}
