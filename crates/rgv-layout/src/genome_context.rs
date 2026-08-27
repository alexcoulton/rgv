use rgv_core::{CytobandStain, GenomeAssembly, GenomicInterval, Viewport};

#[derive(Clone, Debug, PartialEq)]
pub struct RulerTick {
    pub position: u64,
    pub x: f32,
    pub major: bool,
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoordinateRulerLayout {
    pub ticks: Vec<RulerTick>,
    pub span_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoordinateRulerOptions {
    pub minimum_major_spacing: f64,
    pub minor_divisions: u64,
}

impl Default for CoordinateRulerOptions {
    fn default() -> Self {
        Self {
            minimum_major_spacing: 105.0,
            minor_divisions: 5,
        }
    }
}

pub fn layout_coordinate_ruler(
    viewport: Viewport,
    options: CoordinateRulerOptions,
) -> CoordinateRulerLayout {
    let requested_step = viewport.bases_per_pixel() * options.minimum_major_spacing.max(24.0);
    let major_step = nice_integer_step(requested_step);
    let minor_step = (major_step / options.minor_divisions.max(1)).max(1);
    let interval = viewport.interval();
    let first = interval.start().div_ceil(minor_step) * minor_step;
    let mut ticks = Vec::new();
    let mut position = first;
    while position < interval.end() && ticks.len() < 10_000 {
        let major = position.is_multiple_of(major_step);
        ticks.push(RulerTick {
            position,
            x: viewport.pixel_at_genomic(position) as f32,
            major,
            label: major.then(|| format_genomic_position(position.saturating_add(1))),
        });
        let Some(next) = position.checked_add(minor_step) else {
            break;
        };
        position = next;
    }
    CoordinateRulerLayout {
        ticks,
        span_label: format!(
            "{} bp",
            format_genomic_position(viewport.span().round() as u64)
        ),
    }
}

fn nice_integer_step(requested: f64) -> u64 {
    if !requested.is_finite() || requested <= 1.0 {
        return 1;
    }
    let exponent = requested.log10().floor();
    let magnitude = 10_f64.powf(exponent);
    let normalized = requested / magnitude;
    let factor = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    (factor * magnitude).ceil().clamp(1.0, u64::MAX as f64) as u64
}

pub fn format_genomic_position(position: u64) -> String {
    let digits = position.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(char::from(byte));
    }
    formatted
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdeogramSegment {
    pub x: f32,
    pub width: f32,
    pub name: String,
    pub stain: CytobandStain,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdeogramLayout {
    pub segments: Vec<IdeogramSegment>,
    pub visible_x: f32,
    pub visible_width: f32,
    pub has_cytobands: bool,
}

pub fn layout_ideogram(
    assembly: &GenomeAssembly,
    contig: rgv_core::ContigId,
    contig_length: u64,
    visible: GenomicInterval,
    width: f32,
) -> IdeogramLayout {
    let width = width.max(1.0);
    let length = contig_length.max(1) as f64;
    let bands = assembly.cytobands_for(contig).collect::<Vec<_>>();
    let has_cytobands = !bands.is_empty();
    let segments = bands
        .into_iter()
        .map(|band| IdeogramSegment {
            x: (band.interval.start() as f64 / length * f64::from(width)) as f32,
            width: ((band.interval.len() as f64 / length) * f64::from(width)).max(1.0) as f32,
            name: band.name.clone(),
            stain: band.stain,
        })
        .collect();
    let visible_start = visible.start().min(contig_length);
    let visible_end = visible.end().min(contig_length);
    IdeogramLayout {
        segments,
        visible_x: (visible_start as f64 / length * f64::from(width)) as f32,
        visible_width: (((visible_end.saturating_sub(visible_start)) as f64 / length)
            * f64::from(width))
        .max(2.0) as f32,
        has_cytobands,
    }
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, Cytoband, ReferenceDictionary};

    use super::*;

    #[test]
    fn ruler_uses_readable_non_overlapping_major_steps() {
        let interval = GenomicInterval::new(ContigId::new(0), 86_838, 91_760).unwrap();
        let viewport = Viewport::new(interval, 1_800.0).unwrap();
        let layout = layout_coordinate_ruler(viewport, CoordinateRulerOptions::default());
        let major = layout
            .ticks
            .iter()
            .filter(|tick| tick.major)
            .collect::<Vec<_>>();
        assert!(major.len() >= 3);
        assert!(major.windows(2).all(|pair| pair[1].x - pair[0].x >= 100.0));
        assert_eq!(layout.span_label, "4,922 bp");
    }

    #[test]
    fn ideogram_omits_missing_cytobands_and_tracks_visible_span() {
        let contig = ContigId::new(0);
        let assembly = GenomeAssembly::from_dictionary(
            "test",
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000)]),
        );
        let visible = GenomicInterval::new(contig, 250, 500).unwrap();
        let layout = layout_ideogram(&assembly, contig, 1_000, visible, 400.0);
        assert!(!layout.has_cytobands);
        assert!(layout.segments.is_empty());
        assert!((layout.visible_x - 100.0).abs() < f32::EPSILON);
        assert!((layout.visible_width - 100.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ideogram_places_available_cytobands() {
        let contig = ContigId::new(0);
        let mut assembly = GenomeAssembly::from_dictionary(
            "test",
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000)]),
        );
        assembly.cytobands.push(Cytoband {
            interval: GenomicInterval::new(contig, 0, 500).unwrap(),
            name: "p1".to_owned(),
            stain: CytobandStain::Positive50,
        });
        let layout = layout_ideogram(
            &assembly,
            contig,
            1_000,
            GenomicInterval::new(contig, 0, 100).unwrap(),
            400.0,
        );
        assert!(layout.has_cytobands);
        assert!((layout.segments[0].width - 200.0).abs() < f32::EPSILON);
    }

    #[test]
    fn genomic_positions_use_grouped_decimal_digits() {
        assert_eq!(format_genomic_position(1), "1");
        assert_eq!(format_genomic_position(12_345_678), "12,345,678");
    }
}
