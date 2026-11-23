//! Insertion facts for the coverage inspector: how many eligible reads carry an insertion at a
//! base boundary, what fraction of the spanning reads that is, and which inserted sequences are
//! most common. The coverage lane marks the same boundaries (see
//! `rgv_render::coverage_insertion_marks`).

use rgv_model::{CigarOp, CigarOpKind};
use std::collections::HashMap;

/// Inserted sequences listed in the inspector, most common first.
pub(crate) const MAXIMUM_LISTED_INSERTION_SEQUENCES: usize = 3;
/// Longer inserted sequences are shortened in the inspector.
const MAXIMUM_SHOWN_INSERTION_BASES: usize = 24;

/// The bases a read inserts on the boundary after `anchor`, if any. `bases` may be empty when
/// the read has no stored sequence; the insertion is then reported by length only.
pub(crate) fn inserted_bases_after(
    start: u64,
    cigar: &[CigarOp],
    bases: &[u8],
    anchor: u64,
) -> Option<Vec<u8>> {
    let boundary = anchor.checked_add(1)?;
    let mut reference = start;
    let mut read = 0_usize;
    for operation in cigar {
        let length = operation.len as usize;
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                reference = reference.saturating_add(u64::from(operation.len));
                read = read.saturating_add(length);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference = reference.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::Insertion => {
                if reference == boundary {
                    let inserted = bases
                        .get(read..read.saturating_add(length))
                        .map_or_else(|| vec![b'N'; length], <[u8]>::to_vec);
                    return Some(inserted.to_ascii_uppercase());
                }
                read = read.saturating_add(length);
            }
            CigarOpKind::SoftClip => read = read.saturating_add(length),
            CigarOpKind::HardClip | CigarOpKind::Padding => {}
        }
        if reference > boundary {
            break;
        }
    }
    None
}

/// Counts identical inserted sequences, most common first (ties in sequence order).
pub(crate) fn tally_inserted_sequences(
    sequences: impl IntoIterator<Item = Vec<u8>>,
) -> Vec<(Vec<u8>, u32)> {
    let mut counts = HashMap::<Vec<u8>, u32>::new();
    for sequence in sequences {
        let count = counts.entry(sequence).or_default();
        *count = count.saturating_add(1);
    }
    let mut tally = counts.into_iter().collect::<Vec<_>>();
    tally.sort_unstable_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    tally
}

fn shown_sequence(sequence: &[u8]) -> String {
    if sequence.len() <= MAXIMUM_SHOWN_INSERTION_BASES {
        return String::from_utf8_lossy(sequence).into_owned();
    }
    format!(
        "{}… ({} bp)",
        String::from_utf8_lossy(&sequence[..MAXIMUM_SHOWN_INSERTION_BASES]),
        sequence.len()
    )
}

/// The inspector line for one boundary, e.g. `31 of 40 reads (77.5%) · marked (≥ 20%)`.
pub(crate) fn insertion_support_text(
    insertions: u32,
    covering: u32,
    minimum_fraction: Option<f64>,
) -> String {
    let fraction = if covering == 0 {
        0.0
    } else {
        f64::from(insertions) / f64::from(covering)
    };
    let mark = minimum_fraction.map_or_else(String::new, |minimum| {
        if fraction >= minimum {
            format!(" · marked in coverage (≥ {:.0}%)", minimum * 100.0)
        } else {
            format!(" · below the {:.0}% mark threshold", minimum * 100.0)
        }
    });
    format!(
        "{insertions} of {covering} reads ({:.1}%){mark}",
        fraction * 100.0
    )
}

/// The inspector line listing the most common inserted sequences.
pub(crate) fn inserted_sequences_text(tally: &[(Vec<u8>, u32)]) -> String {
    if tally.is_empty() {
        return "unavailable".to_owned();
    }
    let mut parts = tally
        .iter()
        .take(MAXIMUM_LISTED_INSERTION_SEQUENCES)
        .map(|(sequence, count)| format!("+{} ×{count}", shown_sequence(sequence)))
        .collect::<Vec<_>>();
    let others = tally
        .len()
        .saturating_sub(MAXIMUM_LISTED_INSERTION_SEQUENCES);
    if others > 0 {
        parts.push(format!("{others} other sequences"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cigar(operations: &[(CigarOpKind, u32)]) -> Vec<CigarOp> {
        operations
            .iter()
            .map(|(kind, len)| CigarOp {
                kind: *kind,
                len: *len,
            })
            .collect()
    }

    #[test]
    fn inserted_bases_are_found_on_their_anchor_boundary() {
        use CigarOpKind::{Deletion as D, Insertion as I, Match as M, SoftClip as S};
        // 2S 5M 3I 4M from 100: the insertion follows base 104.
        let read = cigar(&[(S, 2), (M, 5), (I, 3), (M, 4)]);
        let bases = b"nnACGTAgatTTTT";
        assert_eq!(
            inserted_bases_after(100, &read, bases, 104),
            Some(b"GAT".to_vec())
        );
        assert_eq!(inserted_bases_after(100, &read, bases, 103), None);
        assert_eq!(inserted_bases_after(100, &read, bases, 105), None);
        // Deletions move the reference, not the read.
        let deleted = cigar(&[(M, 2), (D, 3), (M, 1), (I, 1), (M, 2)]);
        assert_eq!(
            inserted_bases_after(10, &deleted, b"AACTGG", 15),
            Some(b"T".to_vec())
        );
        // Without a stored sequence the length survives as Ns.
        assert_eq!(
            inserted_bases_after(100, &read, b"", 104),
            Some(b"NNN".to_vec())
        );
    }

    #[test]
    fn sequences_are_tallied_most_common_first() {
        let tally = tally_inserted_sequences([
            b"A".to_vec(),
            b"T".to_vec(),
            b"A".to_vec(),
            b"GC".to_vec(),
            b"A".to_vec(),
            b"T".to_vec(),
            b"C".to_vec(),
        ]);
        assert_eq!(tally[0], (b"A".to_vec(), 3));
        assert_eq!(tally[1], (b"T".to_vec(), 2));
        assert_eq!(
            inserted_sequences_text(&tally),
            "+A ×3 · +T ×2 · +C ×1 · 1 other sequences"
        );
        assert_eq!(inserted_sequences_text(&[]), "unavailable");
        let long = tally_inserted_sequences([vec![b'G'; 30]]);
        assert_eq!(
            inserted_sequences_text(&long),
            format!("+{}… (30 bp) ×1", "G".repeat(24))
        );
    }

    #[test]
    fn support_text_reports_count_fraction_and_threshold() {
        assert_eq!(
            insertion_support_text(31, 40, Some(0.2)),
            "31 of 40 reads (77.5%) · marked in coverage (≥ 20%)"
        );
        assert_eq!(
            insertion_support_text(1, 40, Some(0.2)),
            "1 of 40 reads (2.5%) · below the 20% mark threshold"
        );
        assert_eq!(insertion_support_text(0, 0, None), "0 of 0 reads (0.0%)");
    }
}
