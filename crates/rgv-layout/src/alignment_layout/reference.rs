#[allow(clippy::wildcard_imports)]
use super::*;

/// Lays out a standalone reference slice using the same base colours and glyphs
/// as the alignment reference lane.
#[must_use]
pub fn layout_reference_slice(
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
) -> DisplayList {
    layout_reference_slice_oriented(reference, viewport, top, false)
}

#[must_use]
pub fn layout_reference_slice_oriented(
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
    reverse_complement: bool,
) -> DisplayList {
    let mut display_list = DisplayList::default();
    add_reference_bases(
        &mut display_list,
        reference,
        viewport,
        top,
        reverse_complement,
    );
    display_list
}

#[must_use]
pub fn layout_reference_translation(
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
    frame: ReferenceTranslationFrame,
    genetic_code: GeneticCode,
) -> DisplayList {
    let mut display = DisplayList::default();
    let visible_start = reference.interval.start().max(viewport.interval().start());
    let visible_end = reference.interval.end().min(viewport.interval().end());
    let phase = frame.phase();
    let mut start = visible_start.saturating_add((phase + 3 - visible_start % 3) % 3);
    while start.saturating_add(3) <= visible_end {
        let (Some(first), Some(second), Some(third)) = (
            reference.base_at(start),
            reference.base_at(start + 1),
            reference.base_at(start + 2),
        ) else {
            break;
        };
        let mut codon = [first, second, third];
        if frame.is_reverse() {
            codon.reverse();
            for base in &mut codon {
                *base = complement(*base);
            }
        }
        let amino_acid = translate_codon(&codon, genetic_code);
        let (x, width) = clipped_geometry(start, start + 3, viewport);
        display.rectangles.push(RectPrimitive {
            x,
            y: top,
            width: width.max(1.0),
            height: 18.0,
            color: if (start / 3).is_multiple_of(2) {
                Rgba::new(0.90, 0.92, 0.95, 1.0)
            } else {
                Rgba::new(0.96, 0.97, 0.98, 1.0)
            },
            object_id: 0,
        });
        if width >= 12.0 {
            display.texts.push(TextPrimitive {
                x: x + width * 0.5,
                y: top + 9.0,
                text: char::from(amino_acid).to_string(),
                font_size: 12.0,
                color: Rgba::new(0.08, 0.10, 0.14, 1.0),
            });
        }
        start = start.saturating_add(3);
    }
    display
}

pub(crate) fn translate_codon(codon: &[u8], genetic_code: GeneticCode) -> u8 {
    const STANDARD: &[u8; 64] = b"FFLLSSSSYY**CC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG";
    let index = |base: u8| match base.to_ascii_uppercase() {
        b'T' | b'U' => Some(0_usize),
        b'C' => Some(1),
        b'A' => Some(2),
        b'G' => Some(3),
        _ => None,
    };
    if codon.len() < 3 {
        return b'X';
    }
    let (Some(first), Some(second), Some(third)) =
        (index(codon[0]), index(codon[1]), index(codon[2]))
    else {
        return b'X';
    };
    let standard = STANDARD[first * 16 + second * 4 + third];
    if genetic_code == GeneticCode::VertebrateMitochondrial {
        match codon[..3]
            .try_into()
            .map(|codon: [u8; 3]| codon.map(|base| base.to_ascii_uppercase()))
        {
            Ok([b'A', b'T', b'A']) => b'M',
            Ok([b'T', b'G', b'A']) => b'W',
            Ok([b'A', b'G', b'A' | b'G']) => b'*',
            _ => standard,
        }
    } else {
        standard
    }
}
