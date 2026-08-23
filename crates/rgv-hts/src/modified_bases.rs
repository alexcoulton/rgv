use thiserror::Error;

use crate::{AlignmentRecord, AuxField, AuxValue, SequenceData};

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
    pub read_position: usize,
    pub probability: Option<u8>,
}

/// Decode SAM `MM`/`ML` tags into calls located on the stored read sequence.
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
    decode_modified_bases_parts(sequence, &mm.value, ml)
}

/// Decode modified bases from packed-record components without materializing an alignment.
pub fn decode_modified_bases_parts(
    sequence: &[u8],
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
        let (header, raw_deltas) = raw_group
            .split_once(',')
            .ok_or_else(|| ModifiedBaseError::MalformedGroup(raw_group.to_owned()))?;
        let group = parse_group_header(header)?;
        let deltas = raw_deltas
            .split(',')
            .map(|delta| {
                delta
                    .parse::<usize>()
                    .map_err(|_| ModifiedBaseError::InvalidDelta(delta.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let positions = locate_calls(sequence, group.canonical_base, &deltas)?;
        for position in positions {
            for code in &group.codes {
                let probability = probabilities.get(probability_index).copied();
                probability_index += usize::from(!probabilities.is_empty());
                calls.push(ModifiedBaseCall {
                    canonical_base: group.canonical_base,
                    strand: group.strand,
                    code: *code,
                    implicit_mode: group.implicit_mode,
                    read_position: position,
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
    canonical_base: u8,
    deltas: &[usize],
) -> Result<Vec<usize>, ModifiedBaseError> {
    let mut result = Vec::with_capacity(deltas.len());
    let mut cursor = 0;
    for delta in deltas {
        let mut remaining = *delta;
        let position = loop {
            let Some(base) = sequence.get(cursor) else {
                return Err(ModifiedBaseError::DeltaBeyondSequence);
            };
            let position = cursor;
            cursor += 1;
            if base.to_ascii_uppercase() != canonical_base {
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
        assert_eq!(calls[0].read_position, 1);
        assert_eq!(calls[1].read_position, 5);
        assert_eq!(calls[0].probability, Some(240));
        assert_eq!(calls[1].probability, Some(128));
    }

    #[test]
    fn expands_multi_code_groups_in_ml_order() {
        let calls = decode_modified_bases(&record("C+mh?,1;", vec![10, 20])).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].read_position, 2);
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
}
