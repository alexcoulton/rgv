use crate::{DisplayList, RectPrimitive, Rgba, TextPrimitive};
use rgv_core::{GeneticCode, ReferenceTranslationFrame, Viewport};
use rgv_hts::ReferenceSlice;

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
            codon.iter_mut().for_each(|base| *base = complement(*base));
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
            shape: Default::default(),
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

    #[test]
    fn mitochondrial_translation_uses_its_declared_code() {
        assert_eq!(translate_codon(b"ATA", GeneticCode::Standard), b'I');
        assert_eq!(
            translate_codon(b"ATA", GeneticCode::VertebrateMitochondrial),
            b'M'
        );
    }
}
