use rgv_core::SmrtKineticsMode;
use thiserror::Error;

use crate::{AlignmentFlags, AlignmentRecord, AuxValue, SequenceData};

/// Semantic upper bound applied before a SMRT kinetic array is traversed for display or
/// inspection. The packed alignment arenas provide a separate byte bound.
pub const MAX_SMRT_KINETICS_VALUES_PER_RECORD: usize = 10_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SmrtKineticsValues<'a> {
    Signed(&'a [i64]),
    Unsigned(&'a [u64]),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmrtKineticsSeries<'a> {
    pub tag: [u8; 2],
    values: SmrtKineticsValues<'a>,
}

impl SmrtKineticsSeries<'_> {
    pub fn len(self) -> usize {
        match self.values {
            SmrtKineticsValues::Signed(values) => values.len(),
            SmrtKineticsValues::Unsigned(values) => values.len(),
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Returns the kinetic value for a CIGAR-oriented read offset.
    ///
    /// PacBio aligners reverse-complement `SEQ` for a reverse alignment but deliberately leave
    /// the native-orientation kinetics tags unchanged, so these arrays require the reverse index.
    pub fn value_at_aligned_read_position(
        self,
        flags: AlignmentFlags,
        aligned_position: usize,
    ) -> Option<u64> {
        let index = if flags.is_reverse() {
            self.len().checked_sub(aligned_position.checked_add(1)?)?
        } else {
            aligned_position
        };
        match self.values {
            SmrtKineticsValues::Signed(values) => u64::try_from(*values.get(index)?).ok(),
            SmrtKineticsValues::Unsigned(values) => values.get(index).copied(),
        }
    }

    pub fn summary(self) -> SmrtKineticsSummary {
        let mut minimum = u64::MAX;
        let mut maximum = 0_u64;
        let mut sum = 0.0_f64;
        for index in 0..self.len() {
            let value = match self.values {
                SmrtKineticsValues::Signed(values) => {
                    u64::try_from(values[index]).expect("validated SMRT values are nonnegative")
                }
                SmrtKineticsValues::Unsigned(values) => values[index],
            };
            minimum = minimum.min(value);
            maximum = maximum.max(value);
            sum += value as f64;
        }
        SmrtKineticsSummary {
            tag: self.tag,
            values: self.len(),
            minimum: (!self.is_empty()).then_some(minimum),
            maximum: (!self.is_empty()).then_some(maximum),
            mean: (!self.is_empty()).then_some(sum / self.len() as f64),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SmrtKineticsSummary {
    pub tag: [u8; 2],
    pub values: usize,
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
    pub mean: Option<f64>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SmrtKineticsError {
    #[error("sequence is unavailable, so the per-base kinetic array cannot be oriented")]
    SequenceUnavailable,
    #[error("required PacBio kinetic tag {0} is missing")]
    MissingTag(String),
    #[error("PacBio kinetic tag {tag} must be an integer array")]
    UnsupportedTagType { tag: String },
    #[error(
        "PacBio kinetic tag {tag} has {actual} values but the read sequence has {expected} bases"
    )]
    LengthMismatch {
        tag: String,
        expected: usize,
        actual: usize,
    },
    #[error("PacBio kinetic tag {tag} contains a negative value at array index {index}")]
    NegativeValue { tag: String, index: usize },
    #[error(
        "PacBio kinetic tag {tag} has {actual} values, exceeding the {maximum} value safety bound"
    )]
    TooManyValues {
        tag: String,
        actual: usize,
        maximum: usize,
    },
}

pub const fn smrt_kinetics_tag(mode: SmrtKineticsMode, reverse_alignment: bool) -> [u8; 2] {
    match mode {
        SmrtKineticsMode::SubreadIpd => *b"ip",
        SmrtKineticsMode::SubreadPulseWidth => *b"pw",
        SmrtKineticsMode::CcsForwardIpd => {
            if reverse_alignment {
                *b"ri"
            } else {
                *b"fi"
            }
        }
        SmrtKineticsMode::CcsForwardPulseWidth => {
            if reverse_alignment {
                *b"rp"
            } else {
                *b"fp"
            }
        }
        SmrtKineticsMode::CcsReverseIpd => {
            if reverse_alignment {
                *b"fi"
            } else {
                *b"ri"
            }
        }
        SmrtKineticsMode::CcsReversePulseWidth => {
            if reverse_alignment {
                *b"fp"
            } else {
                *b"rp"
            }
        }
    }
}

pub fn decode_smrt_kinetics(
    tag: [u8; 2],
    value: &AuxValue,
    sequence_length: usize,
) -> Result<SmrtKineticsSeries<'_>, SmrtKineticsError> {
    let tag_label = tag_label(tag);
    let values = match value {
        AuxValue::SignedArray(values) => {
            if let Some(index) = values.iter().position(|value| *value < 0) {
                return Err(SmrtKineticsError::NegativeValue {
                    tag: tag_label,
                    index,
                });
            }
            SmrtKineticsValues::Signed(values)
        }
        AuxValue::UnsignedArray(values) => SmrtKineticsValues::Unsigned(values),
        _ => return Err(SmrtKineticsError::UnsupportedTagType { tag: tag_label }),
    };
    let actual = match values {
        SmrtKineticsValues::Signed(values) => values.len(),
        SmrtKineticsValues::Unsigned(values) => values.len(),
    };
    if actual > MAX_SMRT_KINETICS_VALUES_PER_RECORD {
        return Err(SmrtKineticsError::TooManyValues {
            tag: tag_label,
            actual,
            maximum: MAX_SMRT_KINETICS_VALUES_PER_RECORD,
        });
    }
    if actual != sequence_length {
        return Err(SmrtKineticsError::LengthMismatch {
            tag: tag_label,
            expected: sequence_length,
            actual,
        });
    }
    Ok(SmrtKineticsSeries { tag, values })
}

pub fn smrt_kinetics_series(
    record: &AlignmentRecord,
    mode: SmrtKineticsMode,
) -> Result<SmrtKineticsSeries<'_>, SmrtKineticsError> {
    let SequenceData::Present { bases, .. } = &record.sequence else {
        return Err(SmrtKineticsError::SequenceUnavailable);
    };
    let tag = smrt_kinetics_tag(mode, record.flags.is_reverse());
    let field = record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == tag)
        .ok_or_else(|| SmrtKineticsError::MissingTag(tag_label(tag)))?;
    decode_smrt_kinetics(tag, &field.value, bases.len())
}

fn tag_label(tag: [u8; 2]) -> String {
    String::from_utf8_lossy(&tag).into_owned()
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, SmrtKineticsMode};

    use super::*;
    use crate::{AuxField, CigarOp, CigarOpKind};

    fn record(flags: u16, tag: [u8; 2], value: AuxValue) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 0,
            end: 4,
            flags: AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"smrt".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            sequence: SequenceData::Present {
                bases: b"ACGT".to_vec(),
                qualities: None,
            },
            auxiliary_fields: vec![AuxField { tag, value }],
        }
    }

    #[test]
    fn ccs_tag_selection_accounts_for_requested_strand_and_alignment_orientation() {
        assert_eq!(
            smrt_kinetics_tag(SmrtKineticsMode::CcsForwardIpd, false),
            *b"fi"
        );
        assert_eq!(
            smrt_kinetics_tag(SmrtKineticsMode::CcsForwardIpd, true),
            *b"ri"
        );
        assert_eq!(
            smrt_kinetics_tag(SmrtKineticsMode::CcsReversePulseWidth, false),
            *b"rp"
        );
        assert_eq!(
            smrt_kinetics_tag(SmrtKineticsMode::CcsReversePulseWidth, true),
            *b"fp"
        );
    }

    #[test]
    fn reverse_alignment_reads_the_kinetic_array_in_cigar_orientation() {
        let record = record(0x10, *b"ri", AuxValue::UnsignedArray(vec![10, 20, 30, 40]));
        let series = smrt_kinetics_series(&record, SmrtKineticsMode::CcsForwardIpd).unwrap();
        assert_eq!(
            series.value_at_aligned_read_position(record.flags, 0),
            Some(40)
        );
        assert_eq!(
            series.value_at_aligned_read_position(record.flags, 3),
            Some(10)
        );
        assert_eq!(
            series.summary(),
            SmrtKineticsSummary {
                tag: *b"ri",
                values: 4,
                minimum: Some(10),
                maximum: Some(40),
                mean: Some(25.0),
            }
        );
    }

    #[test]
    fn malformed_arrays_fail_explicitly() {
        let mismatched = record(0, *b"ip", AuxValue::UnsignedArray(vec![1, 2, 3]));
        assert!(matches!(
            smrt_kinetics_series(&mismatched, SmrtKineticsMode::SubreadIpd),
            Err(SmrtKineticsError::LengthMismatch {
                expected: 4,
                actual: 3,
                ..
            })
        ));

        let wrong_type = record(0, *b"pw", AuxValue::String("1,2,3,4".to_owned()));
        assert!(matches!(
            smrt_kinetics_series(&wrong_type, SmrtKineticsMode::SubreadPulseWidth),
            Err(SmrtKineticsError::UnsupportedTagType { .. })
        ));
    }
}
