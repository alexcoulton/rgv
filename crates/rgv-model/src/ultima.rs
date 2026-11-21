use rgv_core::UltimaQualityMode;
use thiserror::Error;

use crate::{
    AlignmentFlags, AlignmentRecord, AuxValue, CigarOpKind, ReferenceSlice, SequenceData,
    UltimaTagOrientation,
};

pub const MAX_ULTIMA_VALUES_PER_RECORD: usize = 10_000_000;
const SYMMETRIC_QUALITY_ADJUSTMENT: f32 = 3.010_300_2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UltimaIndelKind {
    Insertion,
    Deletion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UltimaQualitySource {
    TpDirection,
    TpReadMaximumFallback,
    MinimumHomopolymer,
    T0,
    MaximumHomopolymerLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UltimaEventUnavailable {
    NotBracketedByAlignedBases,
    ReferenceUnavailable,
    AmbiguousHomopolymer,
    NonHomopolymer,
    T0Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UltimaIndelEvidence {
    pub operation_index: usize,
    pub kind: UltimaIndelKind,
    pub reference_start: u64,
    pub read_start: usize,
    pub length: usize,
    pub homopolymer_base: Option<u8>,
    pub quality: Option<f32>,
    pub direction: Option<i32>,
    pub quality_source: Option<UltimaQualitySource>,
    pub unavailable: Option<UltimaEventUnavailable>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UltimaSummary {
    pub insertions: usize,
    pub deletions: usize,
    pub quality_events: usize,
    pub directional_events: usize,
    pub t0_events: usize,
    pub minimum_quality_events: usize,
    pub maximum_homopolymer_events: usize,
    pub unavailable_events: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UltimaRecordAnalysis {
    pub events: Vec<UltimaIndelEvidence>,
    pub summary: UltimaSummary,
}

impl UltimaRecordAnalysis {
    pub fn event(&self, operation_index: usize) -> Option<&UltimaIndelEvidence> {
        self.events
            .iter()
            .find(|event| event.operation_index == operation_index)
    }
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum UltimaEvidenceError {
    #[error("read sequence is unavailable")]
    SequenceUnavailable,
    #[error("base qualities are unavailable")]
    QualitiesUnavailable,
    #[error("read sequence has {bases} bases but {qualities} quality values")]
    QualityLengthMismatch { bases: usize, qualities: usize },
    #[error("tp:B:c direction array is missing")]
    MissingTp,
    #[error("tp must be a signed integer array")]
    UnsupportedTpType,
    #[error("tp has {values} values but the read has {bases} bases")]
    TpLengthMismatch { values: usize, bases: usize },
    #[error("tp value {value} at index {index} is outside signed-byte range")]
    TpValueOutOfRange { index: usize, value: i64 },
    #[error("Ultima evidence has {values} values, exceeding the {maximum} value safety bound")]
    TooManyValues { values: usize, maximum: usize },
    #[error("t0 must be a printable Phred+33 string")]
    MalformedT0,
    #[error("t0 has {values} values but the read has {bases} bases")]
    T0LengthMismatch { values: usize, bases: usize },
}

#[allow(clippy::too_many_lines)]
pub fn analyze_ultima_record(
    record: &AlignmentRecord,
    reference: Option<&ReferenceSlice>,
    quality_mode: UltimaQualityMode,
    maximum_homopolymer_length: Option<u16>,
) -> Result<UltimaRecordAnalysis, UltimaEvidenceError> {
    analyze_ultima_record_with_tag_orientation(
        record,
        reference,
        quality_mode,
        maximum_homopolymer_length,
        UltimaTagOrientation::Reference,
    )
}

pub fn analyze_ultima_record_with_tag_orientation(
    record: &AlignmentRecord,
    reference: Option<&ReferenceSlice>,
    quality_mode: UltimaQualityMode,
    maximum_homopolymer_length: Option<u16>,
    tag_orientation: UltimaTagOrientation,
) -> Result<UltimaRecordAnalysis, UltimaEvidenceError> {
    let SequenceData::Present {
        bases,
        qualities: Some(qualities),
    } = &record.sequence
    else {
        return match &record.sequence {
            SequenceData::Present {
                qualities: None, ..
            } => Err(UltimaEvidenceError::QualitiesUnavailable),
            _ => Err(UltimaEvidenceError::SequenceUnavailable),
        };
    };
    if bases.len() != qualities.len() {
        return Err(UltimaEvidenceError::QualityLengthMismatch {
            bases: bases.len(),
            qualities: qualities.len(),
        });
    }
    if bases.len() > MAX_ULTIMA_VALUES_PER_RECORD {
        return Err(UltimaEvidenceError::TooManyValues {
            values: bases.len(),
            maximum: MAX_ULTIMA_VALUES_PER_RECORD,
        });
    }
    let tp = record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == *b"tp")
        .ok_or(UltimaEvidenceError::MissingTp)?;
    let AuxValue::SignedArray(tp) = &tp.value else {
        return Err(UltimaEvidenceError::UnsupportedTpType);
    };
    if tp.len() != bases.len() {
        return Err(UltimaEvidenceError::TpLengthMismatch {
            values: tp.len(),
            bases: bases.len(),
        });
    }
    for (index, value) in tp.iter().copied().enumerate() {
        if i8::try_from(value).is_err() {
            return Err(UltimaEvidenceError::TpValueOutOfRange { index, value });
        }
    }
    let t0 = match record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == *b"t0")
    {
        Some(field) => {
            let AuxValue::String(value) = &field.value else {
                return Err(UltimaEvidenceError::MalformedT0);
            };
            if value.len() != bases.len() {
                return Err(UltimaEvidenceError::T0LengthMismatch {
                    values: value.len(),
                    bases: bases.len(),
                });
            }
            if !value.bytes().all(|value| (33..=126).contains(&value)) {
                return Err(UltimaEvidenceError::MalformedT0);
            }
            Some(value.as_bytes())
        }
        None => None,
    };

    let input = UltimaInput {
        flags: record.flags,
        tag_orientation,
        bases,
        qualities,
        tp,
        t0,
        reference,
        quality_mode,
        maximum_homopolymer_length,
    };
    let mut analysis = UltimaRecordAnalysis::default();
    let mut reference_start = record.start;
    let mut read_start = 0_usize;
    for (operation_index, operation) in record.cigar.iter().enumerate() {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Insertion => {
                analysis.events.push(insertion_evidence(
                    &input,
                    operation_index,
                    reference_start,
                    read_start,
                    length,
                    bracketed_by_aligned_bases(&record.cigar, operation_index),
                ));
                read_start = read_start.saturating_add(length);
            }
            CigarOpKind::Deletion => {
                analysis.events.push(deletion_evidence(
                    &input,
                    operation_index,
                    reference_start,
                    read_start,
                    length,
                    bracketed_by_aligned_bases(&record.cigar, operation_index),
                ));
                reference_start = reference_start.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                read_start = read_start.saturating_add(length);
                reference_start = reference_start.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::SoftClip => read_start = read_start.saturating_add(length),
            CigarOpKind::ReferenceSkip => {
                reference_start = reference_start.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
    }
    analysis.summary = summarize(&analysis.events);
    Ok(analysis)
}

struct UltimaInput<'a> {
    flags: AlignmentFlags,
    tag_orientation: UltimaTagOrientation,
    bases: &'a [u8],
    qualities: &'a [u8],
    tp: &'a [i64],
    t0: Option<&'a [u8]>,
    reference: Option<&'a ReferenceSlice>,
    quality_mode: UltimaQualityMode,
    maximum_homopolymer_length: Option<u16>,
}

#[derive(Clone, Copy)]
struct Hmer {
    start: usize,
    end: usize,
    backwards_size: usize,
    forward_size: usize,
    base: u8,
}

impl Hmer {
    fn size(self) -> usize {
        self.end.saturating_sub(self.start).saturating_add(1)
    }
}

fn insertion_evidence(
    input: &UltimaInput<'_>,
    operation_index: usize,
    reference_start: u64,
    read_start: usize,
    length: usize,
    bracketed: bool,
) -> UltimaIndelEvidence {
    let mut event = unavailable_event(
        operation_index,
        UltimaIndelKind::Insertion,
        reference_start,
        read_start,
        length,
        UltimaEventUnavailable::NotBracketedByAlignedBases,
    );
    if !bracketed {
        return event;
    }
    let Some(read_end) = read_start.checked_add(length) else {
        return event;
    };
    let (Some(first), Some(last), Some(previous), Some(next)) = (
        aligned_base(input, read_start),
        read_end
            .checked_sub(1)
            .and_then(|position| aligned_base(input, position)),
        read_start
            .checked_sub(1)
            .and_then(|position| aligned_base(input, position)),
        aligned_base(input, read_end),
    ) else {
        return event;
    };
    let backwards_hmer = first == previous;
    let forward_hmer = last == next;
    let insertion_is_homopolymer =
        (read_start..read_end).all(|position| aligned_base(input, position) == Some(first));
    let (hmer, direction) = if !backwards_hmer && !forward_hmer && length == 1 {
        (
            Hmer {
                start: read_start,
                end: read_start,
                backwards_size: 0,
                forward_size: 0,
                base: first,
            },
            -1,
        )
    } else if backwards_hmer == forward_hmer {
        event.unavailable = Some(if backwards_hmer {
            UltimaEventUnavailable::AmbiguousHomopolymer
        } else {
            UltimaEventUnavailable::NonHomopolymer
        });
        return event;
    } else if !insertion_is_homopolymer {
        event.unavailable = Some(UltimaEventUnavailable::NonHomopolymer);
        return event;
    } else if forward_hmer {
        let hmer = find_hmer(input, read_end, next, true, true);
        let direction = -i32::try_from(hmer.backwards_size).unwrap_or(i32::MAX);
        (hmer, direction)
    } else {
        let hmer = find_hmer(input, read_start - 1, previous, true, true);
        let direction = -i32::try_from(hmer.forward_size).unwrap_or(i32::MAX);
        (hmer, direction)
    };
    event.homopolymer_base = Some(hmer.base);
    event.direction = Some(direction);
    let (quality, source) = quality_from_hmer(input, hmer, direction);
    event.quality = Some(quality);
    event.quality_source = Some(source);
    event.unavailable = None;
    event
}

#[allow(clippy::too_many_lines)]
fn deletion_evidence(
    input: &UltimaInput<'_>,
    operation_index: usize,
    reference_start: u64,
    read_start: usize,
    length: usize,
    bracketed: bool,
) -> UltimaIndelEvidence {
    let mut event = unavailable_event(
        operation_index,
        UltimaIndelKind::Deletion,
        reference_start,
        read_start,
        length,
        UltimaEventUnavailable::NotBracketedByAlignedBases,
    );
    if !bracketed {
        return event;
    }
    let (Some(previous), Some(next)) = (
        read_start
            .checked_sub(1)
            .and_then(|position| aligned_base(input, position)),
        aligned_base(input, read_start),
    ) else {
        return event;
    };
    let Some(reference) = input.reference else {
        event.unavailable = Some(UltimaEventUnavailable::ReferenceUnavailable);
        return event;
    };
    let Some(reference_end) = reference_start.checked_add(length as u64) else {
        event.unavailable = Some(UltimaEventUnavailable::ReferenceUnavailable);
        return event;
    };
    let Some(gap_first) = reference
        .base_at(reference_start)
        .map(|base| base.to_ascii_uppercase())
    else {
        event.unavailable = Some(UltimaEventUnavailable::ReferenceUnavailable);
        return event;
    };
    let Some(gap_last) = reference_end
        .checked_sub(1)
        .and_then(|position| reference.base_at(position))
        .map(|base| base.to_ascii_uppercase())
    else {
        event.unavailable = Some(UltimaEventUnavailable::ReferenceUnavailable);
        return event;
    };
    let backwards_hmer = gap_first == previous;
    let forward_hmer = gap_last == next;
    let gap_is_homopolymer = (reference_start..reference_end).all(|position| {
        reference
            .base_at(position)
            .is_some_and(|base| base.eq_ignore_ascii_case(&gap_first))
    });
    if backwards_hmer != forward_hmer && gap_is_homopolymer {
        let hmer = if forward_hmer {
            find_hmer(input, read_start, next, false, true)
        } else {
            find_hmer(input, read_start - 1, previous, true, false)
        };
        let direction = i32::try_from(length).unwrap_or(i32::MAX);
        event.homopolymer_base = Some(hmer.base);
        event.direction = Some(direction);
        if input
            .maximum_homopolymer_length
            .is_some_and(|maximum| hmer.size() >= usize::from(maximum))
        {
            event.quality = Some(0.0);
            event.quality_source = Some(UltimaQualitySource::MaximumHomopolymerLimit);
        } else {
            let (quality, source) = quality_from_hmer(input, hmer, direction);
            event.quality = Some(quality);
            event.quality_source = Some(source);
        }
        event.unavailable = None;
        return event;
    }
    if backwards_hmer && forward_hmer {
        event.unavailable = Some(UltimaEventUnavailable::AmbiguousHomopolymer);
        return event;
    }
    if length != 1 {
        event.unavailable = Some(UltimaEventUnavailable::NonHomopolymer);
        return event;
    }
    if input.t0.is_none() {
        event.unavailable = Some(UltimaEventUnavailable::T0Unavailable);
        return event;
    }
    let before = read_start
        .checked_sub(1)
        .and_then(|position| aligned_t0(input, position));
    let after = aligned_t0(input, read_start);
    let quality = match (before, after) {
        (Some(before), Some(after)) => Some(before.max(after)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    if let Some(quality) = quality {
        event.quality = Some(f32::from(quality));
        event.quality_source = Some(UltimaQualitySource::T0);
        event.unavailable = None;
    } else {
        event.unavailable = Some(UltimaEventUnavailable::T0Unavailable);
    }
    event
}

fn quality_from_hmer(
    input: &UltimaInput<'_>,
    hmer: Hmer,
    direction: i32,
) -> (f32, UltimaQualitySource) {
    if input.quality_mode == UltimaQualityMode::Minimum {
        let quality = (hmer.start..=hmer.end)
            .filter_map(|position| aligned_quality(input, position))
            .min()
            .unwrap_or_default();
        return (f32::from(quality), UltimaQualitySource::MinimumHomopolymer);
    }
    if let Some((position, quality)) = (hmer.start..=hmer.end).find_map(|position| {
        (aligned_tp(input, position) == Some(i64::from(direction)))
            .then(|| aligned_quality(input, position).map(|quality| (position, quality)))
            .flatten()
    }) {
        let central = position.saturating_sub(hmer.start) == hmer.end.saturating_sub(position);
        return (
            if central {
                f32::from(quality)
            } else {
                (f32::from(quality) - SYMMETRIC_QUALITY_ADJUSTMENT).max(0.0)
            },
            UltimaQualitySource::TpDirection,
        );
    }
    (
        input.qualities.iter().copied().max().map_or(0.0, f32::from),
        UltimaQualitySource::TpReadMaximumFallback,
    )
}

fn find_hmer(
    input: &UltimaInput<'_>,
    start: usize,
    base: u8,
    walk_backwards: bool,
    walk_forward: bool,
) -> Hmer {
    let mut hmer = Hmer {
        start,
        end: start,
        backwards_size: 0,
        forward_size: 0,
        base,
    };
    if walk_backwards {
        while hmer.start > 0 && aligned_base(input, hmer.start - 1) == Some(base) {
            hmer.start -= 1;
            hmer.backwards_size += 1;
        }
    }
    if walk_forward {
        while hmer.end + 1 < input.bases.len() && aligned_base(input, hmer.end + 1) == Some(base) {
            hmer.end += 1;
            hmer.forward_size += 1;
        }
    }
    hmer
}

fn unavailable_event(
    operation_index: usize,
    kind: UltimaIndelKind,
    reference_start: u64,
    read_start: usize,
    length: usize,
    unavailable: UltimaEventUnavailable,
) -> UltimaIndelEvidence {
    UltimaIndelEvidence {
        operation_index,
        kind,
        reference_start,
        read_start,
        length,
        homopolymer_base: None,
        quality: None,
        direction: None,
        quality_source: None,
        unavailable: Some(unavailable),
    }
}

fn bracketed_by_aligned_bases(cigar: &[crate::CigarOp], operation_index: usize) -> bool {
    let aligned = |kind| {
        matches!(
            kind,
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch
        )
    };
    operation_index
        .checked_sub(1)
        .and_then(|index| cigar.get(index))
        .is_some_and(|operation| aligned(operation.kind))
        && cigar
            .get(operation_index.saturating_add(1))
            .is_some_and(|operation| aligned(operation.kind))
}

fn summarize(events: &[UltimaIndelEvidence]) -> UltimaSummary {
    let mut summary = UltimaSummary::default();
    for event in events {
        match event.kind {
            UltimaIndelKind::Insertion => summary.insertions += 1,
            UltimaIndelKind::Deletion => summary.deletions += 1,
        }
        summary.quality_events += usize::from(event.quality.is_some());
        summary.directional_events +=
            usize::from(event.quality_source == Some(UltimaQualitySource::TpDirection));
        summary.t0_events += usize::from(event.quality_source == Some(UltimaQualitySource::T0));
        summary.minimum_quality_events +=
            usize::from(event.quality_source == Some(UltimaQualitySource::MinimumHomopolymer));
        summary.maximum_homopolymer_events +=
            usize::from(event.quality_source == Some(UltimaQualitySource::MaximumHomopolymerLimit));
        summary.unavailable_events += usize::from(event.unavailable.is_some());
    }
    summary
}

fn aligned_base(input: &UltimaInput<'_>, position: usize) -> Option<u8> {
    input
        .bases
        .get(position)
        .copied()
        .map(|base| base.to_ascii_uppercase())
}

fn aligned_quality(input: &UltimaInput<'_>, position: usize) -> Option<u8> {
    input.qualities.get(position).copied()
}

fn aligned_tp(input: &UltimaInput<'_>, position: usize) -> Option<i64> {
    input
        .tp
        .get(tag_index(input, input.tp.len(), position)?)
        .copied()
}

fn aligned_t0(input: &UltimaInput<'_>, position: usize) -> Option<u8> {
    let t0 = input.t0?;
    t0.get(tag_index(input, t0.len(), position)?)
        .copied()
        .map(|value| value - 33)
}

fn tag_index(input: &UltimaInput<'_>, length: usize, aligned_position: usize) -> Option<usize> {
    if input.tag_orientation == UltimaTagOrientation::Synthesis && input.flags.is_reverse() {
        length.checked_sub(aligned_position.checked_add(1)?)
    } else {
        (aligned_position < length).then_some(aligned_position)
    }
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval};

    use super::*;
    use crate::{AlignmentFlags, AuxField, CigarOp};

    fn record(cigar: &[(CigarOpKind, u32)], bases: &[u8], qualities: &[u8]) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 0,
            end: cigar
                .iter()
                .filter(|(kind, _)| {
                    matches!(
                        kind,
                        CigarOpKind::Match
                            | CigarOpKind::SequenceMatch
                            | CigarOpKind::SequenceMismatch
                            | CigarOpKind::Deletion
                            | CigarOpKind::ReferenceSkip
                    )
                })
                .map(|(_, length)| u64::from(*length))
                .sum(),
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"ultima".to_vec(),
            cigar: cigar
                .iter()
                .map(|(kind, len)| CigarOp {
                    kind: *kind,
                    len: *len,
                })
                .collect(),
            sequence: SequenceData::Present {
                bases: bases.to_vec(),
                qualities: Some(qualities.to_vec()),
            },
            auxiliary_fields: Vec::new(),
        }
    }

    fn add_tp(record: &mut AlignmentRecord, values: &[i64]) {
        record.auxiliary_fields.push(AuxField {
            tag: *b"tp",
            value: AuxValue::SignedArray(values.to_vec()),
        });
    }

    #[test]
    fn forward_homopolymer_and_single_base_insertions_report_quality_and_direction() {
        let mut hmer = record(
            &[
                (CigarOpKind::Match, 3),
                (CigarOpKind::Insertion, 2),
                (CigarOpKind::Match, 3),
            ],
            b"ACGAAATC",
            &[30, 30, 30, 30, 40, 30, 30, 30],
        );
        add_tp(&mut hmer, &[0, 0, 0, 0, -2, 0, 0, 0]);
        let analysis =
            analyze_ultima_record(&hmer, None, UltimaQualityMode::Directional, None).unwrap();
        let event = &analysis.events[0];
        assert_eq!(event.kind, UltimaIndelKind::Insertion);
        assert_eq!(event.homopolymer_base, Some(b'A'));
        assert_eq!(event.direction, Some(-2));
        assert_eq!(event.quality, Some(40.0));
        assert_eq!(event.quality_source, Some(UltimaQualitySource::TpDirection));

        let mut single = record(
            &[
                (CigarOpKind::Match, 3),
                (CigarOpKind::Insertion, 1),
                (CigarOpKind::Match, 3),
            ],
            b"ACGTAAC",
            &[30, 30, 30, 35, 30, 30, 30],
        );
        add_tp(&mut single, &[0, 0, 0, -1, 0, 0, 0]);
        let event = analyze_ultima_record(&single, None, UltimaQualityMode::Directional, None)
            .unwrap()
            .events[0];
        assert_eq!(event.direction, Some(-1));
        assert_eq!(event.quality, Some(35.0));
    }

    #[test]
    fn reverse_alignment_uses_sam_aligned_sequence_quality_and_tp_order() {
        let mut forward = record(
            &[
                (CigarOpKind::Match, 3),
                (CigarOpKind::Insertion, 2),
                (CigarOpKind::Match, 3),
            ],
            b"ACGAAATC",
            &[30, 30, 30, 30, 40, 30, 30, 30],
        );
        add_tp(&mut forward, &[0, 0, 0, 0, -2, 0, 0, 0]);
        let expected = analyze_ultima_record(&forward, None, UltimaQualityMode::Directional, None)
            .unwrap()
            .events[0];

        forward.flags = AlignmentFlags::from_bits(0x10);
        let reverse = analyze_ultima_record(&forward, None, UltimaQualityMode::Directional, None)
            .unwrap()
            .events[0];
        assert_eq!(reverse, expected);
    }

    #[test]
    fn reverse_alignment_reorients_synthesis_oriented_tp_tags_only() {
        let mut reference_oriented = record(
            &[
                (CigarOpKind::Match, 3),
                (CigarOpKind::Insertion, 2),
                (CigarOpKind::Match, 3),
            ],
            b"ACGAAATC",
            &[30, 30, 30, 30, 40, 30, 30, 30],
        );
        reference_oriented.flags = AlignmentFlags::from_bits(0x10);
        add_tp(&mut reference_oriented, &[0, 0, 0, 0, -2, 0, 0, 0]);
        let expected = analyze_ultima_record_with_tag_orientation(
            &reference_oriented,
            None,
            UltimaQualityMode::Directional,
            None,
            UltimaTagOrientation::Reference,
        )
        .unwrap();

        let mut synthesis_oriented = reference_oriented.clone();
        let AuxValue::SignedArray(tp) = &mut synthesis_oriented.auxiliary_fields[0].value else {
            unreachable!();
        };
        tp.reverse();
        let observed = analyze_ultima_record_with_tag_orientation(
            &synthesis_oriented,
            None,
            UltimaQualityMode::Directional,
            None,
            UltimaTagOrientation::Synthesis,
        )
        .unwrap();
        assert_eq!(observed, expected);
    }

    #[test]
    fn deletion_uses_tp_maximum_hmer_limit_and_t0_sources() {
        let mut hmer = record(
            &[
                (CigarOpKind::Match, 3),
                (CigarOpKind::Deletion, 2),
                (CigarOpKind::Match, 3),
            ],
            b"ACGAAA",
            &[30, 30, 30, 30, 38, 30],
        );
        add_tp(&mut hmer, &[0, 0, 0, 0, 2, 0]);
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 8).unwrap(),
            bases: b"ACGAAAAA".to_vec(),
        };
        let event = analyze_ultima_record(
            &hmer,
            Some(&reference),
            UltimaQualityMode::Directional,
            None,
        )
        .unwrap()
        .events[0];
        assert_eq!(event.direction, Some(2));
        assert_eq!(event.quality, Some(38.0));

        let limited = analyze_ultima_record(
            &hmer,
            Some(&reference),
            UltimaQualityMode::Directional,
            Some(3),
        )
        .unwrap()
        .events[0];
        assert_eq!(limited.quality, Some(0.0));
        assert_eq!(
            limited.quality_source,
            Some(UltimaQualitySource::MaximumHomopolymerLimit)
        );

        let mut single = record(
            &[
                (CigarOpKind::Match, 3),
                (CigarOpKind::Deletion, 1),
                (CigarOpKind::Match, 3),
            ],
            b"ACGAAC",
            &[30; 6],
        );
        add_tp(&mut single, &[0; 6]);
        single.auxiliary_fields.push(AuxField {
            tag: *b"t0",
            value: AuxValue::String(
                [10_u8, 10, 20, 30, 10, 10]
                    .map(|quality| char::from(quality + 33))
                    .iter()
                    .collect(),
            ),
        });
        let reference = ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), 0, 7).unwrap(),
            bases: b"ACGTAAC".to_vec(),
        };
        let event = analyze_ultima_record(
            &single,
            Some(&reference),
            UltimaQualityMode::Directional,
            None,
        )
        .unwrap()
        .events[0];
        assert_eq!(event.direction, None);
        assert_eq!(event.quality, Some(30.0));

        single.flags = AlignmentFlags::from_bits(0x10);
        let expected = analyze_ultima_record_with_tag_orientation(
            &single,
            Some(&reference),
            UltimaQualityMode::Directional,
            None,
            UltimaTagOrientation::Reference,
        )
        .unwrap();
        let mut synthesis_oriented = single.clone();
        let AuxValue::SignedArray(tp) = &mut synthesis_oriented.auxiliary_fields[0].value else {
            unreachable!();
        };
        tp.reverse();
        let AuxValue::String(t0) = &mut synthesis_oriented.auxiliary_fields[1].value else {
            unreachable!();
        };
        *t0 = t0.chars().rev().collect();
        let observed = analyze_ultima_record_with_tag_orientation(
            &synthesis_oriented,
            Some(&reference),
            UltimaQualityMode::Directional,
            None,
            UltimaTagOrientation::Synthesis,
        )
        .unwrap();
        assert_eq!(observed, expected);
        assert_eq!(event.quality_source, Some(UltimaQualitySource::T0));
    }

    #[test]
    fn directional_adjustment_minimum_mode_and_fallback_are_exact() {
        let mut record = record(
            &[
                (CigarOpKind::Match, 3),
                (CigarOpKind::Insertion, 2),
                (CigarOpKind::Match, 3),
            ],
            b"ACGAAATC",
            &[30, 30, 30, 40, 22, 35, 30, 30],
        );
        add_tp(&mut record, &[0, 0, 0, -2, 0, 0, 0, 0]);
        let directional =
            analyze_ultima_record(&record, None, UltimaQualityMode::Directional, None)
                .unwrap()
                .events[0];
        assert!((directional.quality.unwrap() - 36.9897).abs() < 0.001);

        let minimum = analyze_ultima_record(&record, None, UltimaQualityMode::Minimum, None)
            .unwrap()
            .events[0];
        assert_eq!(minimum.quality, Some(22.0));
        assert_eq!(
            minimum.quality_source,
            Some(UltimaQualitySource::MinimumHomopolymer)
        );

        let AuxValue::SignedArray(tp) = &mut record.auxiliary_fields[0].value else {
            unreachable!();
        };
        tp.fill(0);
        let fallback = analyze_ultima_record(&record, None, UltimaQualityMode::Directional, None)
            .unwrap()
            .events[0];
        assert_eq!(fallback.quality, Some(40.0));
        assert_eq!(
            fallback.quality_source,
            Some(UltimaQualitySource::TpReadMaximumFallback)
        );
    }

    #[test]
    fn missing_reference_and_malformed_tags_are_explicit() {
        let mut record = record(
            &[
                (CigarOpKind::Match, 3),
                (CigarOpKind::Deletion, 1),
                (CigarOpKind::Match, 3),
            ],
            b"ACGAAC",
            &[30; 6],
        );
        assert_eq!(
            analyze_ultima_record(&record, None, UltimaQualityMode::Directional, None),
            Err(UltimaEvidenceError::MissingTp)
        );
        add_tp(&mut record, &[0; 5]);
        assert!(matches!(
            analyze_ultima_record(&record, None, UltimaQualityMode::Directional, None),
            Err(UltimaEvidenceError::TpLengthMismatch { .. })
        ));
        record.auxiliary_fields[0].value = AuxValue::SignedArray(vec![0; 6]);
        let event = analyze_ultima_record(&record, None, UltimaQualityMode::Directional, None)
            .unwrap()
            .events[0];
        assert_eq!(
            event.unavailable,
            Some(UltimaEventUnavailable::ReferenceUnavailable)
        );
    }
}
