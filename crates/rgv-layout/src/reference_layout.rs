use crate::{DisplayList, RectPrimitive, Rgba, TextPrimitive};
use rgv_core::{GeneticCode, ReferenceTranslationFrame, Viewport};
use rgv_model::ReferenceSlice;

pub const MINIMUM_BASE_GLYPH_PIXEL_WIDTH: f32 = 7.0;

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
    let mut display = DisplayList::default();
    let visible_start = reference.interval.start().max(viewport.interval().start());
    let visible_end = reference.interval.end().min(viewport.interval().end());
    for position in visible_start..visible_end {
        let Some(mut base) = reference.base_at(position) else {
            continue;
        };
        if reverse_complement {
            base = complement(base);
        }
        let (x, width) = clipped_geometry(position, position.saturating_add(1), viewport);
        if width <= 0.0 {
            continue;
        }
        let color = base_color(base);
        display.rectangles.push(RectPrimitive {
            x,
            y: top,
            width: width.max(1.0),
            height: 20.0,
            color: Rgba::new(color.red, color.green, color.blue, 0.32),
            object_id: 0,
            shape: Default::default(),
        });
        if width >= MINIMUM_BASE_GLYPH_PIXEL_WIDTH {
            display.texts.push(TextPrimitive {
                x: x + width * 0.5,
                y: top + 10.0,
                text: char::from(base.to_ascii_uppercase()).to_string(),
                font_size: 12.6,
                color: Rgba::new(color.red * 0.5, color.green * 0.5, color.blue * 0.5, 1.0),
            });
        }
    }
    display
}

/// Lays out one translation frame using forward-strand phase for every
/// frame. Prefer [`layout_reference_translation_in_contig`], which places
/// reverse frames relative to the contig end.
#[must_use]
pub fn layout_reference_translation(
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
    frame: ReferenceTranslationFrame,
    genetic_code: GeneticCode,
) -> DisplayList {
    layout_reference_translation_in_contig(reference, viewport, top, frame, genetic_code, None)
}

/// Lays out one translation frame.
///
/// Forward frame *n* reads codons starting at zero-based contig positions
/// congruent to `n - 1` modulo three. Reverse frame *n* is frame *n* of the
/// reverse-complemented contig, so its codons are anchored at the contig
/// *end*: with `contig_length` supplied, a reverse codon occupies
/// `[start, start + 3)` where `contig_length - (start + 3)` is congruent to
/// `n - 1` modulo three. Without a contig length the forward phase is reused,
/// which labels reverse frames correctly only when the length is a multiple
/// of three.
///
/// Every codon that intersects the viewport is drawn, clipped to it. A codon
/// straddling the viewport edge is translated when the slice holds all three
/// of its bases and is otherwise drawn as an unlabelled block.
#[must_use]
pub fn layout_reference_translation_in_contig(
    reference: &ReferenceSlice,
    viewport: Viewport,
    top: f32,
    frame: ReferenceTranslationFrame,
    genetic_code: GeneticCode,
    contig_length: Option<u64>,
) -> DisplayList {
    let mut display = DisplayList::default();
    let visible_start = reference.interval.start().max(viewport.interval().start());
    let visible_end = reference.interval.end().min(viewport.interval().end());
    // Codon starts are the positions congruent to `phase` modulo three.
    let phase = match contig_length {
        Some(length) if frame.is_reverse() => (length % 3 + 3 - frame.phase()) % 3,
        _ => frame.phase(),
    };
    // First codon that intersects the viewport, which may begin up to two
    // bases before it. Bases before the first codon start of the contig
    // belong to no codon and are skipped.
    let offset = (visible_start % 3 + 3 - phase) % 3;
    let mut start = if visible_start >= offset {
        visible_start - offset
    } else {
        visible_start + (3 - offset)
    };
    while start < visible_end {
        // The one or two bases left over at the contig end form no codon.
        if contig_length.is_some_and(|length| start.saturating_add(3) > length) {
            break;
        }
        let (x, width) = clipped_geometry(start, start.saturating_add(3), viewport);
        if width <= 0.0 {
            start = start.saturating_add(3);
            continue;
        }
        let amino_acid = match (
            reference.base_at(start),
            reference.base_at(start + 1),
            reference.base_at(start + 2),
        ) {
            (Some(first), Some(second), Some(third)) => {
                let mut codon = [first, second, third];
                if frame.is_reverse() {
                    codon.reverse();
                    codon.iter_mut().for_each(|base| *base = complement(*base));
                }
                Some(translate_codon(&codon, genetic_code))
            }
            // The slice ends inside this codon: its residue is unknown.
            _ => None,
        };
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
            shape: Default::default(),
        });
        if let Some(amino_acid) = amino_acid
            && width >= 12.0
        {
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

fn clipped_geometry(start: u64, end: u64, viewport: Viewport) -> (f32, f32) {
    let interval = viewport.interval();
    let start = start.max(interval.start()).min(interval.end());
    let end = end.max(interval.start()).min(interval.end());
    if start >= end {
        return (0.0, 0.0);
    }
    let x = viewport.pixel_at_genomic(start) as f32;
    let right = viewport.pixel_at_genomic(end) as f32;
    (x, right - x)
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

const fn base_color(base: u8) -> Rgba {
    match base.to_ascii_uppercase() {
        b'A' => Rgba::new(0.24, 0.78, 0.38, 1.0),
        b'C' => Rgba::new(0.22, 0.52, 0.96, 1.0),
        b'G' => Rgba::new(0.96, 0.65, 0.18, 1.0),
        b'T' | b'U' => Rgba::new(0.94, 0.28, 0.28, 1.0),
        _ => Rgba::new(0.60, 0.64, 0.70, 1.0),
    }
}

const fn translate_codon(codon: &[u8], genetic_code: GeneticCode) -> u8 {
    const STANDARD: &[u8; 64] = b"FFLLSSSSYY**CC*WLLLLPPPPHHQQRRRRIIIMTTTTNNKKSSRRVVVVAAAADDEEGGGG";
    const fn base_index(base: u8) -> Option<usize> {
        match base.to_ascii_uppercase() {
            b'T' | b'U' => Some(0),
            b'C' => Some(1),
            b'A' => Some(2),
            b'G' => Some(3),
            _ => None,
        }
    }
    if codon.len() < 3 {
        return b'X';
    }
    let (Some(first), Some(second), Some(third)) = (
        base_index(codon[0]),
        base_index(codon[1]),
        base_index(codon[2]),
    ) else {
        return b'X';
    };
    let standard = STANDARD[first * 16 + second * 4 + third];
    if matches!(genetic_code, GeneticCode::VertebrateMitochondrial) {
        match [
            codon[0].to_ascii_uppercase(),
            codon[1].to_ascii_uppercase(),
            codon[2].to_ascii_uppercase(),
        ] {
            [b'A', b'T', b'A'] => b'M',
            [b'T', b'G', b'A'] => b'W',
            [b'A', b'G', b'A' | b'G'] => b'*',
            _ => standard,
        }
    } else {
        standard
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rgv_core::{ContigId, GenomicInterval};

    fn slice(start: u64, bases: &[u8]) -> ReferenceSlice {
        ReferenceSlice {
            interval: GenomicInterval::new(ContigId::new(0), start, start + bases.len() as u64)
                .unwrap(),
            bases: bases.to_vec(),
        }
    }

    fn view(start: u64, end: u64) -> Viewport {
        let interval = GenomicInterval::new(ContigId::new(0), start, end).unwrap();
        Viewport::new(interval, (end - start) as f64 * 20.0).unwrap()
    }

    fn residues(display: &DisplayList) -> String {
        display
            .texts
            .iter()
            .map(|text| text.text.as_str())
            .collect()
    }

    #[test]
    fn codons_straddling_the_viewport_edges_are_drawn_clipped() {
        // ATG GCC TAA at 0..9; the viewport 1..8 cuts the first and last codon.
        let reference = slice(0, b"ATGGCCTAA");
        let display = layout_reference_translation_in_contig(
            &reference,
            view(1, 8),
            0.0,
            ReferenceTranslationFrame::Forward1,
            GeneticCode::Standard,
            Some(9),
        );
        assert_eq!(residues(&display), "MA*");
        let blocks = display
            .rectangles
            .iter()
            .map(|rectangle| (rectangle.x, rectangle.width))
            .collect::<Vec<_>>();
        assert_eq!(blocks, [(0.0, 40.0), (40.0, 60.0), (100.0, 40.0)]);

        // The legacy entry point shares the fix.
        let legacy = layout_reference_translation(
            &reference,
            view(1, 8),
            0.0,
            ReferenceTranslationFrame::Forward1,
            GeneticCode::Standard,
        );
        assert_eq!(legacy, display);

        // A codon the slice only partly covers is a block without a residue,
        // and later codons are still translated.
        let partial = slice(1, b"TGGCCTAA");
        let display = layout_reference_translation_in_contig(
            &partial,
            view(1, 9),
            0.0,
            ReferenceTranslationFrame::Forward1,
            GeneticCode::Standard,
            None,
        );
        assert_eq!(display.rectangles.len(), 3);
        assert_eq!(residues(&display), "A*");
    }

    #[test]
    fn reverse_frames_are_phased_from_the_contig_end() {
        // Contig of ten bases: reverse frame 1 reads the reverse complement
        // from its first base, i.e. codons end at position 10, 7, 4.
        //   forward:            A C A T G G C C T A
        //   reverse complement: T A G G C C A T G T
        let reference = slice(0, b"ACATGGCCTA");
        let layout = |frame, contig_length| {
            layout_reference_translation_in_contig(
                &reference,
                view(0, 10),
                0.0,
                frame,
                GeneticCode::Standard,
                contig_length,
            )
        };
        let starts = |display: &DisplayList| {
            display
                .rectangles
                .iter()
                .map(|rectangle| (rectangle.x / 20.0).round() as u64)
                .collect::<Vec<_>>()
        };

        let reverse1 = layout(ReferenceTranslationFrame::Reverse1, Some(10));
        // Codons [1,4) [4,7) [7,10); base 0 is left over and belongs to none.
        assert_eq!(starts(&reverse1), [1, 4, 7]);
        // Read 5'->3' on the reverse strand: TAG GCC ATG = * A M, drawn left to right as M A *.
        assert_eq!(residues(&reverse1), "MA*");

        let reverse2 = layout(ReferenceTranslationFrame::Reverse2, Some(10));
        assert_eq!(starts(&reverse2), [0, 3, 6]);
        // AGG CCA TGT = R P C, drawn left to right as C P R.
        assert_eq!(residues(&reverse2), "CPR");

        let reverse3 = layout(ReferenceTranslationFrame::Reverse3, Some(10));
        assert_eq!(starts(&reverse3), [2, 5]);

        // Without a contig length the forward phase is reused (legacy).
        let legacy = layout(ReferenceTranslationFrame::Reverse1, None);
        assert_eq!(starts(&legacy), [0, 3, 6, 9]);
        assert_eq!(
            legacy,
            layout_reference_translation(
                &reference,
                view(0, 10),
                0.0,
                ReferenceTranslationFrame::Reverse1,
                GeneticCode::Standard,
            )
        );
        // Forward frames ignore the contig length.
        assert_eq!(
            layout(ReferenceTranslationFrame::Forward2, Some(10)),
            layout(ReferenceTranslationFrame::Forward2, None)
        );
        // A contig whose length is a multiple of three agrees with the forward phase.
        let nine = slice(0, b"ACATGGCCT");
        let frame = ReferenceTranslationFrame::Reverse1;
        assert_eq!(
            layout_reference_translation_in_contig(
                &nine,
                view(0, 9),
                0.0,
                frame,
                GeneticCode::Standard,
                Some(9)
            ),
            layout_reference_translation(&nine, view(0, 9), 0.0, frame, GeneticCode::Standard)
        );
    }

    #[test]
    fn mitochondrial_translation_uses_its_declared_code() {
        assert_eq!(translate_codon(b"ATA", GeneticCode::Standard), b'I');
        assert_eq!(
            translate_codon(b"ATA", GeneticCode::VertebrateMitochondrial),
            b'M'
        );
    }
}
