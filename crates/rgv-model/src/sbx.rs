use thiserror::Error;

use crate::{AlignmentFlags, AlignmentRecord, CigarOpKind, SequenceData};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SbxInsertionState {
    Concordant,
    PartlyDiscordant,
    FullyDiscordant,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SbxTailBounds {
    pub read_length: usize,
    pub retained_start: usize,
    pub retained_end: usize,
    pub left_tail_bases: usize,
    pub right_tail_bases: usize,
}

impl SbxTailBounds {
    pub const fn retained_bases(self) -> usize {
        self.retained_end.saturating_sub(self.retained_start)
    }

    pub const fn all_tail(self) -> bool {
        self.read_length > 0 && self.retained_bases() == 0
    }

    pub const fn is_tail(self, aligned_read_position: usize) -> bool {
        aligned_read_position < self.retained_start || aligned_read_position >= self.retained_end
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SbxSummary {
    pub left_tail_bases: usize,
    pub right_tail_bases: usize,
    pub retained_bases: usize,
    pub concordant_insertions: usize,
    pub partly_discordant_insertions: usize,
    pub fully_discordant_insertions: usize,
    pub unavailable_insertions: usize,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SbxEvidenceError {
    #[error("read sequence is unavailable")]
    SequenceUnavailable,
    #[error("base qualities are unavailable")]
    QualitiesUnavailable,
    #[error("read sequence has {bases} bases but {qualities} quality values")]
    LengthMismatch { bases: usize, qualities: usize },
    #[error("insertion range {start}..{end} is outside the {read_length}-base read")]
    InvalidInsertionRange {
        start: usize,
        end: usize,
        read_length: usize,
    },
}

pub fn sbx_tail_bounds(
    _flags: AlignmentFlags,
    bases: &[u8],
    qualities: &[u8],
    tail_quality_maximum: u8,
) -> Result<SbxTailBounds, SbxEvidenceError> {
    validate_lengths(bases, qualities)?;
    let mut retained_start = 0_usize;
    while retained_start < bases.len()
        && aligned_quality(qualities, retained_start)
            .is_some_and(|quality| quality <= tail_quality_maximum)
    {
        retained_start += 1;
    }
    let mut retained_end = bases.len();
    while retained_end > retained_start
        && aligned_quality(qualities, retained_end - 1)
            .is_some_and(|quality| quality <= tail_quality_maximum)
    {
        retained_end -= 1;
    }
    Ok(SbxTailBounds {
        read_length: bases.len(),
        retained_start,
        retained_end,
        left_tail_bases: retained_start,
        right_tail_bases: bases.len().saturating_sub(retained_end),
    })
}

pub fn classify_sbx_insertion(
    _flags: AlignmentFlags,
    bases: &[u8],
    qualities: &[u8],
    insertion_start: usize,
    insertion_length: usize,
    discordant_quality_maximum: u8,
) -> Result<SbxInsertionState, SbxEvidenceError> {
    validate_lengths(bases, qualities)?;
    let insertion_end = insertion_start.checked_add(insertion_length).ok_or(
        SbxEvidenceError::InvalidInsertionRange {
            start: insertion_start,
            end: usize::MAX,
            read_length: bases.len(),
        },
    )?;
    if insertion_length == 0 || insertion_end > bases.len() {
        return Err(SbxEvidenceError::InvalidInsertionRange {
            start: insertion_start,
            end: insertion_end,
            read_length: bases.len(),
        });
    }
    let insertion_qualities = (insertion_start..insertion_end)
        .filter_map(|position| aligned_quality(qualities, position));
    if insertion_qualities
        .clone()
        .all(|quality| quality <= discordant_quality_maximum)
    {
        return Ok(SbxInsertionState::FullyDiscordant);
    }
    if insertion_qualities
        .clone()
        .any(|quality| quality <= discordant_quality_maximum)
        || insertion_start.checked_sub(1).is_some_and(|position| {
            aligned_quality(qualities, position)
                .is_some_and(|quality| quality <= discordant_quality_maximum)
        })
        || aligned_quality(qualities, insertion_end)
            .is_some_and(|quality| quality <= discordant_quality_maximum)
        || homopolymer_has_discordant_base(
            bases,
            qualities,
            insertion_start,
            insertion_end,
            discordant_quality_maximum,
        )
    {
        Ok(SbxInsertionState::PartlyDiscordant)
    } else {
        Ok(SbxInsertionState::Concordant)
    }
}

pub fn summarize_sbx_record(
    record: &AlignmentRecord,
    discordant_quality_maximum: u8,
    tail_quality_maximum: u8,
) -> Result<SbxSummary, SbxEvidenceError> {
    let SequenceData::Present {
        bases,
        qualities: Some(qualities),
    } = &record.sequence
    else {
        return match &record.sequence {
            SequenceData::Present {
                qualities: None, ..
            } => Err(SbxEvidenceError::QualitiesUnavailable),
            _ => Err(SbxEvidenceError::SequenceUnavailable),
        };
    };
    let tails = sbx_tail_bounds(record.flags, bases, qualities, tail_quality_maximum)?;
    let mut summary = SbxSummary {
        left_tail_bases: tails.left_tail_bases,
        right_tail_bases: tails.right_tail_bases,
        retained_bases: tails.retained_bases(),
        ..SbxSummary::default()
    };
    let mut read_position = 0_usize;
    for operation in &record.cigar {
        match operation.kind {
            CigarOpKind::Insertion => {
                match classify_sbx_insertion(
                    record.flags,
                    bases,
                    qualities,
                    read_position,
                    operation.len as usize,
                    discordant_quality_maximum,
                ) {
                    Ok(SbxInsertionState::Concordant) => summary.concordant_insertions += 1,
                    Ok(SbxInsertionState::PartlyDiscordant) => {
                        summary.partly_discordant_insertions += 1;
                    }
                    Ok(SbxInsertionState::FullyDiscordant) => {
                        summary.fully_discordant_insertions += 1;
                    }
                    Err(_) => summary.unavailable_insertions += 1,
                }
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Match
            | CigarOpKind::SequenceMatch
            | CigarOpKind::SequenceMismatch
            | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion
            | CigarOpKind::ReferenceSkip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {}
        }
    }
    Ok(summary)
}

fn homopolymer_has_discordant_base(
    bases: &[u8],
    qualities: &[u8],
    insertion_start: usize,
    insertion_end: usize,
    threshold: u8,
) -> bool {
    let Some(base) = aligned_base(bases, insertion_start) else {
        return false;
    };
    if (insertion_start + 1..insertion_end)
        .any(|position| aligned_base(bases, position) != Some(base))
    {
        return false;
    }
    let mut left = insertion_start;
    while left > 0 && aligned_base(bases, left - 1) == Some(base) {
        left -= 1;
    }
    let mut right = insertion_end;
    while right < bases.len() && aligned_base(bases, right) == Some(base) {
        right += 1;
    }
    (left..right).any(|position| {
        aligned_quality(qualities, position).is_some_and(|quality| quality <= threshold)
    })
}

fn validate_lengths(bases: &[u8], qualities: &[u8]) -> Result<(), SbxEvidenceError> {
    if bases.len() == qualities.len() {
        Ok(())
    } else {
        Err(SbxEvidenceError::LengthMismatch {
            bases: bases.len(),
            qualities: qualities.len(),
        })
    }
}

fn aligned_base(bases: &[u8], position: usize) -> Option<u8> {
    bases
        .get(position)
        .copied()
        .map(|base| base.to_ascii_uppercase())
}

fn aligned_quality(qualities: &[u8], position: usize) -> Option<u8> {
    qualities.get(position).copied()
}

#[cfg(test)]
mod tests {
    use rgv_core::ContigId;

    use super::*;
    use crate::{CigarOp, CigarOpKind};

    #[test]
    fn tail_detection_uses_sam_alignment_quality_order() {
        let forward = sbx_tail_bounds(
            AlignmentFlags::from_bits(0),
            b"ACGTAC",
            &[20, 25, 39, 39, 39, 5],
            30,
        )
        .unwrap();
        assert_eq!((forward.left_tail_bases, forward.right_tail_bases), (2, 1));
        assert_eq!((forward.retained_start, forward.retained_end), (2, 5));

        let reverse = sbx_tail_bounds(
            AlignmentFlags::from_bits(0x10),
            b"ACGTAC",
            &[20, 25, 39, 39, 39, 5],
            30,
        )
        .unwrap();
        assert_eq!((reverse.left_tail_bases, reverse.right_tail_bases), (2, 1));
        assert_eq!((reverse.retained_start, reverse.retained_end), (2, 5));
    }

    #[test]
    fn insertion_states_cover_full_partial_adjacent_homopolymer_and_concordant_evidence() {
        let flags = AlignmentFlags::from_bits(0);
        assert_eq!(
            classify_sbx_insertion(flags, b"ACGT", &[39, 5, 10, 39], 1, 2, 10).unwrap(),
            SbxInsertionState::FullyDiscordant
        );
        assert_eq!(
            classify_sbx_insertion(flags, b"ACGT", &[39, 39, 5, 39], 1, 2, 10).unwrap(),
            SbxInsertionState::PartlyDiscordant
        );
        assert_eq!(
            classify_sbx_insertion(flags, b"ACGT", &[5, 39, 39, 39], 1, 2, 10).unwrap(),
            SbxInsertionState::PartlyDiscordant
        );
        assert_eq!(
            classify_sbx_insertion(flags, b"AAAAA", &[5, 39, 39, 39, 39], 1, 2, 10).unwrap(),
            SbxInsertionState::PartlyDiscordant
        );
        assert_eq!(
            classify_sbx_insertion(flags, b"ACGT", &[39; 4], 1, 2, 10).unwrap(),
            SbxInsertionState::Concordant
        );
        assert_eq!(
            classify_sbx_insertion(
                AlignmentFlags::from_bits(0x10),
                b"ACGT",
                &[39, 10, 5, 39],
                1,
                2,
                10,
            )
            .unwrap(),
            SbxInsertionState::FullyDiscordant
        );
    }

    #[test]
    fn malformed_or_missing_sbx_evidence_is_explicit() {
        assert!(matches!(
            sbx_tail_bounds(AlignmentFlags::from_bits(0), b"ACGT", &[30; 3], 30),
            Err(SbxEvidenceError::LengthMismatch {
                bases: 4,
                qualities: 3
            })
        ));
        assert!(matches!(
            classify_sbx_insertion(AlignmentFlags::from_bits(0), b"ACGT", &[30; 4], 4, 1, 10,),
            Err(SbxEvidenceError::InvalidInsertionRange { .. })
        ));
        assert!(matches!(
            classify_sbx_insertion(AlignmentFlags::from_bits(0), b"ACGT", &[30; 4], 1, 0, 10,),
            Err(SbxEvidenceError::InvalidInsertionRange { .. })
        ));

        let mut record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 0,
            end: 4,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"missing".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            sequence: SequenceData::Missing,
            auxiliary_fields: Vec::new(),
        };
        assert_eq!(
            summarize_sbx_record(&record, 10, 30),
            Err(SbxEvidenceError::SequenceUnavailable)
        );
        record.sequence = SequenceData::Present {
            bases: b"ACGT".to_vec(),
            qualities: None,
        };
        assert_eq!(
            summarize_sbx_record(&record, 10, 30),
            Err(SbxEvidenceError::QualitiesUnavailable)
        );
    }

    #[test]
    fn summary_counts_tails_and_insertion_states() {
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 0,
            end: 4,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"sbx".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 2,
                },
                CigarOp {
                    kind: CigarOpKind::Insertion,
                    len: 2,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 2,
                },
            ],
            sequence: SequenceData::Present {
                bases: b"AACCGG".to_vec(),
                qualities: Some(vec![20, 39, 5, 10, 39, 25]),
            },
            auxiliary_fields: Vec::new(),
        };
        let summary = summarize_sbx_record(&record, 10, 30).unwrap();
        assert_eq!(summary.left_tail_bases, 1);
        assert_eq!(summary.right_tail_bases, 1);
        assert_eq!(summary.fully_discordant_insertions, 1);
        assert_eq!(summary.retained_bases, 4);
    }
}
