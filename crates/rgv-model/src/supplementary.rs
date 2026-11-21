use crate::{AlignmentRecord, AuxValue, CigarOp, CigarOpKind};

pub const MAX_SUPPLEMENTARY_SEGMENTS: usize = 64;
pub const MAX_SUPPLEMENTARY_CIGAR_OPERATIONS: usize = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupplementarySegment {
    pub reference_name: String,
    pub start: u64,
    pub end: u64,
    pub reverse: bool,
    pub cigar: Vec<CigarOp>,
    pub mapping_quality: Option<u8>,
    pub edit_distance: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SupplementaryChain {
    pub tag_present: bool,
    pub segments: Vec<SupplementarySegment>,
    pub malformed_entries: usize,
    pub truncated: bool,
    pub invalid_tag_type: bool,
}

/// Parses the SAM `SA` tag without letting a malformed entry hide valid siblings.
pub fn decode_supplementary_chain(record: &AlignmentRecord) -> SupplementaryChain {
    let Some(field) = record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == *b"SA")
    else {
        return SupplementaryChain::default();
    };
    let AuxValue::String(value) = &field.value else {
        return SupplementaryChain {
            tag_present: true,
            invalid_tag_type: true,
            ..SupplementaryChain::default()
        };
    };
    let mut chain = SupplementaryChain {
        tag_present: true,
        ..SupplementaryChain::default()
    };
    for raw in value.split(';').filter(|entry| !entry.is_empty()) {
        if chain.segments.len() >= MAX_SUPPLEMENTARY_SEGMENTS {
            chain.truncated = true;
            continue;
        }
        match parse_segment(raw) {
            Some(segment) => chain.segments.push(segment),
            None => chain.malformed_entries = chain.malformed_entries.saturating_add(1),
        }
    }
    chain
}

fn parse_segment(raw: &str) -> Option<SupplementarySegment> {
    let fields = raw.split(',').collect::<Vec<_>>();
    let [
        reference_name,
        position,
        strand,
        cigar,
        mapping_quality,
        edit_distance,
    ] = fields.as_slice()
    else {
        return None;
    };
    if reference_name.is_empty() {
        return None;
    }
    let one_based_start = position.parse::<u64>().ok()?;
    if one_based_start == 0 {
        return None;
    }
    let reverse = match *strand {
        "+" => false,
        "-" => true,
        _ => return None,
    };
    let cigar = parse_cigar(cigar)?;
    let reference_length = cigar.iter().fold(0_u64, |length, operation| {
        if matches!(
            operation.kind,
            CigarOpKind::Match
                | CigarOpKind::Deletion
                | CigarOpKind::ReferenceSkip
                | CigarOpKind::SequenceMatch
                | CigarOpKind::SequenceMismatch
        ) {
            length.saturating_add(u64::from(operation.len))
        } else {
            length
        }
    });
    let mapping_quality = mapping_quality
        .parse::<u8>()
        .ok()
        .and_then(|value| (value != u8::MAX).then_some(value));
    Some(SupplementarySegment {
        reference_name: (*reference_name).to_owned(),
        start: one_based_start - 1,
        end: (one_based_start - 1)
            .saturating_add(reference_length)
            .max(one_based_start),
        reverse,
        cigar,
        mapping_quality,
        edit_distance: edit_distance.parse().ok()?,
    })
}

fn parse_cigar(raw: &str) -> Option<Vec<CigarOp>> {
    if raw.is_empty() || raw == "*" {
        return None;
    }
    let mut operations = Vec::new();
    let mut length = 0_u64;
    for byte in raw.bytes() {
        if byte.is_ascii_digit() {
            length = length
                .checked_mul(10)?
                .checked_add(u64::from(byte - b'0'))?;
            continue;
        }
        if length == 0 || operations.len() >= MAX_SUPPLEMENTARY_CIGAR_OPERATIONS {
            return None;
        }
        let kind = match byte {
            b'M' => CigarOpKind::Match,
            b'I' => CigarOpKind::Insertion,
            b'D' => CigarOpKind::Deletion,
            b'N' => CigarOpKind::ReferenceSkip,
            b'S' => CigarOpKind::SoftClip,
            b'H' => CigarOpKind::HardClip,
            b'P' => CigarOpKind::Padding,
            b'=' => CigarOpKind::SequenceMatch,
            b'X' => CigarOpKind::SequenceMismatch,
            _ => return None,
        };
        operations.push(CigarOp {
            kind,
            len: u32::try_from(length).ok()?,
        });
        length = 0;
    }
    (length == 0 && !operations.is_empty()).then_some(operations)
}

#[cfg(test)]
mod tests {
    use rgv_core::ContigId;

    use super::*;
    use crate::{AlignmentFlags, AuxField, SequenceData};

    fn record(value: AuxValue) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 100,
            end: 200,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"split".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 100,
            }],
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: vec![AuxField { tag: *b"SA", value }],
        }
    }

    #[test]
    fn parses_all_cigar_kinds_and_sam_coordinates() {
        let chain = decode_supplementary_chain(&record(AuxValue::String(
            "chr2,101,-,5S10M2I3D4N6=7X8H9P,42,5;".to_owned(),
        )));
        assert!(chain.tag_present);
        assert_eq!(chain.segments.len(), 1);
        let segment = &chain.segments[0];
        assert_eq!(segment.reference_name, "chr2");
        assert_eq!((segment.start, segment.end), (100, 130));
        assert!(segment.reverse);
        assert_eq!(segment.mapping_quality, Some(42));
        assert_eq!(segment.edit_distance, 5);
        assert_eq!(segment.cigar.len(), 9);
    }

    #[test]
    fn malformed_entries_do_not_hide_valid_siblings() {
        let chain = decode_supplementary_chain(&record(AuxValue::String(
            "broken;chr2,101,+,100M,60,1;chr3,0,-,5M,20,0;".to_owned(),
        )));
        assert_eq!(chain.segments.len(), 1);
        assert_eq!(chain.malformed_entries, 2);
        assert!(!chain.truncated);
    }

    #[test]
    fn segment_count_is_bounded_and_reports_truncation() {
        let value = (0..MAX_SUPPLEMENTARY_SEGMENTS + 3).fold(String::new(), |mut value, index| {
            use std::fmt::Write as _;
            write!(value, "chr2,{},+,10M,60,0;", index + 1).unwrap();
            value
        });
        let chain = decode_supplementary_chain(&record(AuxValue::String(value)));
        assert_eq!(chain.segments.len(), MAX_SUPPLEMENTARY_SEGMENTS);
        assert!(chain.truncated);
    }

    #[test]
    fn invalid_type_is_explicit() {
        let chain = decode_supplementary_chain(&record(AuxValue::Unsigned(1)));
        assert!(chain.tag_present);
        assert!(chain.invalid_tag_type);
        assert!(chain.segments.is_empty());
    }
}
