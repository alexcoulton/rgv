use rgv_core::BisulfiteContext;

use crate::{AlignmentFlags, AlignmentRecord, CigarOpKind, ReferenceSlice, SequenceData};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BisulfiteObservation {
    Protected,
    Converted,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BisulfiteSummary {
    pub protected: u64,
    pub converted: u64,
    pub other_context_bases: u64,
    pub aligned_bases_examined: u64,
}

impl BisulfiteSummary {
    pub const fn context_observations(self) -> u64 {
        self.protected
            .saturating_add(self.converted)
            .saturating_add(self.other_context_bases)
    }
}

/// Strand used by conversion-aware modes. Paired reads follow the first end's strand so the two
/// records from one template receive one consistent interpretation.
pub fn bisulfite_strand_is_reverse(flags: AlignmentFlags) -> bool {
    if flags.is_first_in_template() {
        flags.is_reverse()
    } else if flags.is_last_in_template() {
        flags.is_mate_reverse()
    } else {
        flags.is_reverse()
    }
}

pub fn classify_bisulfite_observation(
    reference: &ReferenceSlice,
    position: u64,
    reverse: bool,
    context: BisulfiteContext,
    observed_base: u8,
) -> Option<BisulfiteObservation> {
    if !bisulfite_context_matches(reference, position, reverse, context) {
        return None;
    }
    match (reverse, observed_base.to_ascii_uppercase()) {
        (false, b'C') | (true, b'G') => Some(BisulfiteObservation::Protected),
        (false, b'T') | (true, b'A') => Some(BisulfiteObservation::Converted),
        _ => None,
    }
}

pub fn bisulfite_context_matches(
    reference: &ReferenceSlice,
    position: u64,
    reverse: bool,
    context: BisulfiteContext,
) -> bool {
    let base = |offset| strand_reference_base(reference, position, reverse, offset);
    match context {
        BisulfiteContext::Cg => matches!((base(0), base(1)), (Some(b'C'), Some(b'G'))),
        BisulfiteContext::Chg => {
            matches!((base(0), base(1), base(2)), (Some(b'C'), Some(next), Some(b'G')) if is_h(next))
        }
        BisulfiteContext::Chh => {
            matches!((base(0), base(1), base(2)), (Some(b'C'), Some(one), Some(two)) if is_h(one) && is_h(two))
        }
        BisulfiteContext::Hcg => {
            matches!((base(-1), base(0), base(1)), (Some(previous), Some(b'C'), Some(b'G')) if is_h(previous))
        }
        BisulfiteContext::Gch => {
            matches!((base(-1), base(0), base(1)), (Some(b'G'), Some(b'C'), Some(next)) if is_h(next))
        }
        BisulfiteContext::Wcg => {
            matches!((base(-1), base(0), base(1)), (Some(previous), Some(b'C'), Some(b'G')) if is_w(previous))
        }
    }
}

pub fn summarize_bisulfite_record(
    record: &AlignmentRecord,
    reference: &ReferenceSlice,
    context: BisulfiteContext,
) -> BisulfiteSummary {
    let SequenceData::Present { bases, .. } = &record.sequence else {
        return BisulfiteSummary::default();
    };
    let reverse = bisulfite_strand_is_reverse(record.flags);
    let mut summary = BisulfiteSummary::default();
    let mut reference_position = record.start;
    let mut read_position = 0_usize;
    for operation in &record.cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                for offset in 0..length {
                    let position = reference_position.saturating_add(offset as u64);
                    let Some(base) =
                        oriented_base(record.flags, bases, read_position.saturating_add(offset))
                    else {
                        continue;
                    };
                    summary.aligned_bases_examined =
                        summary.aligned_bases_examined.saturating_add(1);
                    if !bisulfite_context_matches(reference, position, reverse, context) {
                        continue;
                    }
                    match classify_bisulfite_observation(
                        reference, position, reverse, context, base,
                    ) {
                        Some(BisulfiteObservation::Protected) => {
                            summary.protected = summary.protected.saturating_add(1);
                        }
                        Some(BisulfiteObservation::Converted) => {
                            summary.converted = summary.converted.saturating_add(1);
                        }
                        None => {
                            summary.other_context_bases =
                                summary.other_context_bases.saturating_add(1);
                        }
                    }
                }
                reference_position = reference_position.saturating_add(u64::from(operation.len));
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                read_position = read_position.saturating_add(length);
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    summary
}

fn strand_reference_base(
    reference: &ReferenceSlice,
    position: u64,
    reverse: bool,
    strand_offset: i64,
) -> Option<u8> {
    let genomic_offset = if reverse {
        strand_offset.checked_neg()?
    } else {
        strand_offset
    };
    let genomic_position = position.checked_add_signed(genomic_offset)?;
    let base = reference.base_at(genomic_position)?.to_ascii_uppercase();
    Some(if reverse { complement(base) } else { base })
}

fn oriented_base(flags: AlignmentFlags, bases: &[u8], aligned_position: usize) -> Option<u8> {
    if flags.is_reverse() {
        let index = bases
            .len()
            .checked_sub(aligned_position.saturating_add(1))?;
        Some(complement(*bases.get(index)?))
    } else {
        bases.get(aligned_position).copied()
    }
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

const fn is_h(base: u8) -> bool {
    matches!(base, b'A' | b'C' | b'T')
}

const fn is_w(base: u8) -> bool {
    matches!(base, b'A' | b'T')
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval};

    use super::*;
    use crate::{CigarOp, SequenceData};

    fn record(sequence: &[u8], flags: u16) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 0,
            end: sequence.len() as u64,
            flags: AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"bisulfite".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: sequence.len() as u32,
            }],
            sequence: SequenceData::Present {
                bases: sequence.to_vec(),
                qualities: None,
            },
            auxiliary_fields: Vec::new(),
        }
    }

    #[test]
    fn summary_counts_conversion_states_and_noncanonical_context_bases() {
        let interval = GenomicInterval::new(ContigId::new(0), 0, 8).unwrap();
        let reference = ReferenceSlice {
            interval,
            bases: b"ACGTCGAA".to_vec(),
        };
        let summary =
            summarize_bisulfite_record(&record(b"ACGTNGAA", 0), &reference, BisulfiteContext::Cg);
        assert_eq!(summary.protected, 1);
        assert_eq!(summary.converted, 0);
        assert_eq!(summary.other_context_bases, 1);
        assert_eq!(summary.context_observations(), 2);
        assert_eq!(summary.aligned_bases_examined, 8);
    }

    #[test]
    fn second_end_uses_first_of_pair_strand() {
        let flags = AlignmentFlags::from_bits(0x1 | 0x20 | 0x80);
        assert!(bisulfite_strand_is_reverse(flags));
    }
}
