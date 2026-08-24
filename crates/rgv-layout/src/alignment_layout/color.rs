#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn smrt_kinetics_color(value: u64) -> Rgba {
    let value = value.min(6_000) as f32;
    if value <= 100.0 {
        let alpha = 0.08 + value / 100.0 * 0.90;
        Rgba::new(0.94, 0.08, 0.06, alpha)
    } else if value <= 600.0 {
        let fraction = (value - 100.0) / 500.0;
        Rgba::new(0.96, fraction * 0.94, 0.04, 0.98)
    } else {
        let fraction = (value - 600.0) / 5_400.0;
        Rgba::new(0.96 * (1.0 - fraction), 0.94, fraction * 0.94, 0.98)
    }
}

pub(crate) fn bisulfite_observation(
    query: &AlignmentQuery,
    index: usize,
    context: BisulfiteContext,
    position: u64,
    observed_base: u8,
) -> Option<BisulfiteObservation> {
    let reference = query.reference.as_ref()?;
    classify_bisulfite_observation(
        reference,
        position,
        bisulfite_strand_is_reverse(query.batch.flags[index]),
        context,
        observed_base,
    )
}

pub(crate) fn oriented_base(
    flags: AlignmentFlags,
    bases: &[u8],
    qualities: Option<&[u8]>,
    aligned_position: usize,
) -> Option<(u8, Option<u8>)> {
    if flags.is_reverse() {
        let index = bases
            .len()
            .checked_sub(aligned_position.saturating_add(1))?;
        Some((
            complement(*bases.get(index)?),
            qualities.and_then(|values| values.get(index)).copied(),
        ))
    } else {
        Some((
            *bases.get(aligned_position)?,
            qualities
                .and_then(|values| values.get(aligned_position))
                .copied(),
        ))
    }
}

pub(crate) fn visible_positions(
    interval: rgv_core::GenomicInterval,
    viewport: Viewport,
) -> std::ops::Range<u64> {
    let start = interval.start().max(viewport.interval().start());
    let end = interval.end().min(viewport.interval().end());
    start..end
}

pub(crate) fn complement(base: u8) -> u8 {
    match base.to_ascii_uppercase() {
        b'A' => b'T',
        b'C' => b'G',
        b'G' => b'C',
        b'T' | b'U' => b'A',
        other => other,
    }
}

pub(crate) fn base_color(base: u8) -> Rgba {
    match base.to_ascii_uppercase() {
        b'A' => Rgba::new(0.24, 0.78, 0.38, 1.0),
        b'C' => Rgba::new(0.22, 0.52, 0.96, 1.0),
        b'G' => Rgba::new(0.96, 0.65, 0.18, 1.0),
        b'T' | b'U' => Rgba::new(0.94, 0.28, 0.28, 1.0),
        _ => Rgba::new(0.60, 0.64, 0.70, 1.0),
    }
}

pub(crate) fn reference_base_label_color(base: u8) -> Rgba {
    let color = base_color(base);
    Rgba::new(color.red * 0.5, color.green * 0.5, color.blue * 0.5, 1.0)
}

pub(crate) fn read_base_label_color(base: u8) -> Rgba {
    let color = base_color(base);
    fill_label_color(color)
}

pub(crate) fn fill_label_color(color: Rgba) -> Rgba {
    let luminance = color.red * 0.2126 + color.green * 0.7152 + color.blue * 0.0722;
    if luminance >= 0.55 {
        Rgba::new(0.06, 0.08, 0.10, 1.0)
    } else {
        Rgba::new(0.98, 0.99, 1.0, 1.0)
    }
}

pub(crate) const fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba::new(color.red, color.green, color.blue, alpha)
}

pub(crate) fn add_base_glyph(
    display_list: &mut DisplayList,
    base: u8,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Rgba,
) {
    display_list.texts.push(TextPrimitive {
        x: x + width * 0.5,
        y: y + height * 0.5,
        text: char::from(base.to_ascii_uppercase()).to_string(),
        font_size: (height * 0.90).clamp(10.0, 16.0),
        color,
    });
}

pub(crate) fn add_read_body(
    display_list: &mut DisplayList,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    fill: Rgba,
    object_id: u32,
) {
    display_list.rectangles.push(RectPrimitive {
        x,
        y,
        width,
        height,
        color: Rgba::new(0.60, 0.62, 0.65, 0.92),
        object_id,
    });
    let inset = 0.8;
    if width > inset * 2.0 && height > inset * 2.0 {
        display_list.rectangles.push(RectPrimitive {
            x: x + inset,
            y: y + inset,
            width: width - inset * 2.0,
            height: height - inset * 2.0,
            color: fill,
            object_id,
        });
    }
}

pub(crate) fn add_event_label(
    display_list: &mut DisplayList,
    label: &str,
    center_x: f32,
    center_y: f32,
    color: Rgba,
    _object_id: u32,
    budget: &mut usize,
) {
    if *budget == 0 || label.is_empty() {
        return;
    }
    *budget -= 1;
    let text = label.chars().take(12).collect::<String>();
    let width = text.chars().count() as f32 * 5.5;
    let left = center_x - width * 0.5;
    let top = center_y - 6.0;
    display_list.rectangles.push(RectPrimitive {
        x: left - 2.0,
        y: top,
        width: width + 4.0,
        height: 12.0,
        color: Rgba::new(1.0, 1.0, 1.0, 0.96),
        object_id: 0,
    });
    display_list.texts.push(TextPrimitive {
        x: center_x,
        y: center_y,
        text,
        font_size: 9.0,
        color,
    });
}

pub(crate) fn add_marker(
    display_list: &mut DisplayList,
    viewport: Viewport,
    reference_position: u64,
    y: f32,
    height: f32,
    color: Rgba,
    object_id: u32,
) {
    let x = viewport.pixel_at_genomic(reference_position) as f32;
    if x >= -2.0 && x <= viewport.pixel_width() as f32 + 2.0 {
        display_list.rectangles.push(RectPrimitive {
            x: x - 1.0,
            y: y - 1.0,
            width: 2.0,
            height: height + 2.0,
            color,
            object_id: read_detail_object_id(object_id),
        });
    }
}

pub(crate) fn add_ultima_insertion_quality_mark(
    display_list: &mut DisplayList,
    viewport: Viewport,
    reference_position: u64,
    y: f32,
    height: f32,
    quality: f32,
    object_id: u32,
) {
    let x = viewport.pixel_at_genomic(reference_position) as f32;
    if x < -3.0 || x > viewport.pixel_width() as f32 + 3.0 {
        return;
    }
    let stroke = 2.0_f32.min(height.max(1.0));
    let width = if height > 10.0 {
        6.0
    } else if height > 5.0 {
        4.0
    } else {
        2.0
    };
    let fraction = 1.0 - quality.clamp(0.0, 42.0) / 42.0;
    display_list.rectangles.push(RectPrimitive {
        x: x - width * 0.5,
        y: y + (height - stroke).max(0.0) * fraction,
        width,
        height: stroke,
        color: ultima_quality_color(quality),
        object_id: read_detail_object_id(object_id),
    });
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn add_ultima_deletion_quality_mark(
    display_list: &mut DisplayList,
    x: f32,
    width: f32,
    y: f32,
    height: f32,
    quality: f32,
    object_id: u32,
) {
    let stroke = 2.0_f32.min(width.max(1.0));
    let fraction = quality.clamp(0.0, 42.0) / 42.0;
    display_list.rectangles.push(RectPrimitive {
        x: x + width * fraction - stroke * 0.5,
        y,
        width: stroke,
        height,
        color: ultima_quality_color(quality),
        object_id: read_detail_object_id(object_id),
    });
}

/// IGV's 44-entry Ultima jet scale, reproduced with the same integer arithmetic.
pub fn ultima_quality_color(quality: f32) -> Rgba {
    const SIZE: usize = 44;
    let index = quality.clamp(0.0, (SIZE - 1) as f32) as usize;
    let green = ultima_jet_green(index);
    let blue = if index >= SIZE * 7 / 8 {
        0
    } else {
        ultima_jet_green((index + SIZE / 4) % SIZE)
    };
    let red = if index < SIZE / 8 {
        0
    } else {
        ultima_jet_green((index + SIZE * 6 / 8) % SIZE)
    };
    Rgba::new(
        f32::from(red) / 255.0,
        f32::from(green) / 255.0,
        f32::from(blue) / 255.0,
        1.0,
    )
}

pub(crate) fn ultima_jet_green(index: usize) -> u8 {
    const SIZE: usize = 44;
    if !(SIZE / 8..SIZE * 7 / 8).contains(&index) {
        0
    } else if index < SIZE * 3 / 8 {
        u8::try_from(255 * (index - SIZE / 8) * 4 / SIZE).unwrap_or(u8::MAX)
    } else if index < SIZE * 5 / 8 {
        u8::MAX
    } else {
        u8::try_from(255 - 255 * (index - SIZE * 5 / 8) * 4 / SIZE).unwrap_or_default()
    }
}

pub(crate) fn clipped_geometry(start: u64, end: u64, viewport: Viewport) -> (f32, f32) {
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

#[allow(clippy::too_many_lines)]
pub(crate) fn alignment_color(
    query: &AlignmentQuery,
    index: usize,
    mode: AlignmentColorMode,
    pair_length_bounds: TemplateLengthBounds,
    alpha: f32,
) -> Rgba {
    let batch = &query.batch;
    let mapq_fraction = batch.mapping_qualities[index]
        .map_or(0.45, |value| (f32::from(value) / 60.0).clamp(0.2, 1.0));
    match mode {
        AlignmentColorMode::None => {
            let grey = 0.82 + mapq_fraction * 0.12;
            Rgba::new(grey, grey, grey, alpha.max(0.92))
        }
        AlignmentColorMode::Strand => {
            if batch.flags[index].is_reverse() {
                Rgba::new(0.58, 0.36, 0.67, alpha)
            } else {
                Rgba::new(0.92, 0.55, 0.64, alpha)
            }
        }
        AlignmentColorMode::MappingQuality => Rgba::new(
            0.22 + mapq_fraction * 0.58,
            0.28 + mapq_fraction * 0.62,
            0.34 + mapq_fraction * 0.56,
            alpha,
        ),
        AlignmentColorMode::ReadGroup => record_tag_label(query, index, *b"RG")
            .map_or(Rgba::new(0.56, 0.60, 0.68, alpha), |label| {
                palette_color(&label, alpha)
            }),
        AlignmentColorMode::Haplotype => record_tag_label(query, index, *b"HP")
            .map_or(Rgba::new(0.56, 0.60, 0.68, alpha), |label| {
                palette_color(&label, alpha)
            }),
        AlignmentColorMode::TemplateLength => {
            let Some(pair) = pair_evidence_at(batch, index, query.interval, pair_length_bounds)
            else {
                return Rgba::new(0.56, 0.60, 0.68, alpha);
            };
            if matches!(
                pair.classification,
                PairClassification::DiscordantTemplateLengthShort
                    | PairClassification::DiscordantTemplateLengthLong
            ) {
                Rgba::new(0.90, 0.27, 0.22, alpha)
            } else if let (Some(minimum), Some(maximum)) =
                (pair_length_bounds.minimum, pair_length_bounds.maximum)
            {
                let span = maximum.saturating_sub(minimum).max(1) as f32;
                let fraction = (pair.absolute_template_length.saturating_sub(minimum) as f32
                    / span)
                    .clamp(0.0, 1.0);
                Rgba::new(
                    0.20 + fraction * 0.48,
                    0.68 - fraction * 0.22,
                    0.82 - fraction * 0.18,
                    alpha,
                )
            } else {
                Rgba::new(0.56, 0.60, 0.68, alpha)
            }
        }
        AlignmentColorMode::PairOrientation => {
            match pair_evidence_at(batch, index, query.interval, pair_length_bounds)
                .map(|pair| pair.orientation)
            {
                Some(PairOrientation::Inward) => Rgba::new(0.35, 0.68, 0.48, alpha),
                Some(PairOrientation::Outward) => Rgba::new(0.91, 0.45, 0.18, alpha),
                Some(PairOrientation::SameStrand) => Rgba::new(0.62, 0.36, 0.76, alpha),
                Some(PairOrientation::Unknown) | None => neutral_alignment_color(alpha),
            }
        }
        AlignmentColorMode::UnexpectedPair => {
            match pair_evidence_at(batch, index, query.interval, pair_length_bounds)
                .map(|pair| pair.classification)
            {
                Some(PairClassification::Proper | PairClassification::Concordant) => {
                    Rgba::new(0.72, 0.76, 0.80, alpha)
                }
                Some(PairClassification::Unpaired) | None => neutral_alignment_color(alpha),
                Some(_) => Rgba::new(0.91, 0.29, 0.24, alpha),
            }
        }
        AlignmentColorMode::MateContig => batch.mate_contigs[index]
            .map_or(Rgba::new(0.56, 0.60, 0.68, alpha), |contig| {
                palette_color(&format!("tid {}", contig.get()), alpha)
            }),
        AlignmentColorMode::FirstOfPairStrand => {
            let flags = batch.flags[index];
            let reverse = if flags.is_first_in_template() {
                Some(flags.is_reverse())
            } else if flags.is_last_in_template() {
                Some(flags.is_mate_reverse())
            } else {
                None
            };
            match reverse {
                Some(true) => Rgba::new(0.58, 0.36, 0.67, alpha),
                Some(false) => Rgba::new(0.92, 0.55, 0.64, alpha),
                None => Rgba::new(0.56, 0.60, 0.68, alpha),
            }
        }
        AlignmentColorMode::Bisulfite(_) => {
            let reverse = first_of_pair_reverse(batch.flags[index])
                .unwrap_or_else(|| batch.flags[index].is_reverse());
            if reverse {
                // A muted sage body keeps the protected/converted red and blue marks dominant.
                Rgba::new(0.68, 0.75, 0.62, alpha.max(0.92))
            } else {
                Rgba::new(0.82, 0.82, 0.82, alpha.max(0.92))
            }
        }
        AlignmentColorMode::SmrtKinetics(_) => {
            // Keep missing and low-value kinetic observations visibly attached to the read.
            Rgba::new(0.80, 0.82, 0.84, alpha.max(0.92))
        }
        AlignmentColorMode::Ultima(_) | AlignmentColorMode::Sbx(_) => {
            Rgba::new(0.82, 0.83, 0.85, alpha.max(0.92))
        }
        AlignmentColorMode::Tag(tag) => record_tag_label(query, index, tag)
            .map_or(Rgba::new(0.56, 0.60, 0.68, alpha), |label| {
                palette_color(&label, alpha)
            }),
    }
}

fn neutral_alignment_color(alpha: f32) -> Rgba {
    // Sparse neutral reads need to remain legible against the white canvas;
    // the previous 0.56/0.60/0.68 body was easily lost at regional scale.
    Rgba::new(0.38, 0.43, 0.52, alpha.max(0.94))
}

pub(crate) fn record_tag_label(
    query: &AlignmentQuery,
    index: usize,
    tag: [u8; 2],
) -> Option<String> {
    let field = query.batch.auxiliary_field(index, tag).ok()??;
    let value = &field.value;
    Some(match value {
        AuxValue::Character(value) => char::from(*value).to_string(),
        AuxValue::Signed(value) => value.to_string(),
        AuxValue::Unsigned(value) => value.to_string(),
        AuxValue::Float(value) => value.to_string(),
        AuxValue::Double(value) => value.to_string(),
        AuxValue::String(value) | AuxValue::HexByteArray(value) => value.clone(),
        AuxValue::SignedArray(_) | AuxValue::UnsignedArray(_) | AuxValue::FloatArray(_) => {
            return None;
        }
    })
}

pub(crate) fn palette_color(label: &str, alpha: f32) -> Rgba {
    const PALETTE: [[f32; 3]; 8] = [
        [0.26, 0.70, 0.94],
        [0.94, 0.40, 0.42],
        [0.42, 0.82, 0.48],
        [0.80, 0.48, 0.94],
        [0.96, 0.68, 0.24],
        [0.24, 0.78, 0.76],
        [0.94, 0.42, 0.74],
        [0.66, 0.72, 0.30],
    ];
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in label.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let color = PALETTE[hash as usize % PALETTE.len()];
    Rgba::new(color[0], color[1], color[2], alpha)
}
