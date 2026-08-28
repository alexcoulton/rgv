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
    pub start: u64,
    pub end: u64,
    pub name: String,
    pub stain: CytobandStain,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IdeogramCentromere {
    pub x: f32,
    pub width: f32,
    pub waist_x: f32,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdeogramLayout {
    pub segments: Vec<IdeogramSegment>,
    pub centromere: Option<IdeogramCentromere>,
    pub visible_x: f32,
    pub visible_width: f32,
    pub has_cytobands: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContigAxis {
    contig_length: u64,
    left: f32,
    width: f32,
}

impl ContigAxis {
    #[must_use]
    pub fn new(contig_length: u64, left: f32, width: f32) -> Self {
        Self {
            contig_length: contig_length.max(1),
            left,
            width: width.max(1.0),
        }
    }

    #[must_use]
    pub const fn contig_length(self) -> u64 {
        self.contig_length
    }

    #[must_use]
    pub const fn left(self) -> f32 {
        self.left
    }

    #[must_use]
    pub const fn width(self) -> f32 {
        self.width
    }

    #[must_use]
    pub fn right(self) -> f32 {
        self.left + self.width
    }

    #[must_use]
    pub fn x_at_boundary(self, boundary: u64) -> f32 {
        self.left
            + (boundary.min(self.contig_length) as f64 / self.contig_length as f64
                * f64::from(self.width)) as f32
    }

    #[must_use]
    pub fn x_at_fraction(self, numerator: usize, denominator: usize) -> f32 {
        if denominator == 0 {
            return self.left;
        }
        self.left + numerator.min(denominator) as f32 / denominator as f32 * self.width
    }

    #[must_use]
    pub fn position_at_x(self, x: f32) -> u64 {
        let fraction = ((x - self.left) / self.width).clamp(0.0, 1.0);
        (f64::from(fraction) * self.contig_length as f64)
            .floor()
            .min(self.contig_length.saturating_sub(1) as f64) as u64
    }

    #[must_use]
    pub fn interval_geometry(self, start: u64, end: u64, minimum_width: f32) -> (f32, f32) {
        let x = self.x_at_boundary(start);
        let right = self.x_at_boundary(end);
        (x, (right - x).max(minimum_width))
    }
}

pub fn layout_ideogram(
    assembly: &GenomeAssembly,
    contig: rgv_core::ContigId,
    visible: GenomicInterval,
    axis: ContigAxis,
) -> IdeogramLayout {
    let bands = assembly.cytobands_for(contig).collect::<Vec<_>>();
    let has_cytobands = !bands.is_empty();
    let segments = bands
        .into_iter()
        .map(|band| {
            let (x, width) =
                axis.interval_geometry(band.interval.start(), band.interval.end(), 1.0);
            IdeogramSegment {
                x,
                width,
                start: band.interval.start(),
                end: band.interval.end(),
                name: band.name.clone(),
                stain: band.stain,
            }
        })
        .collect::<Vec<_>>();
    let centromere = ideogram_centromere(&segments, axis);
    let visible_start = visible.start().min(axis.contig_length());
    let visible_end = visible.end().min(axis.contig_length());
    let (visible_x, visible_width) = axis.interval_geometry(visible_start, visible_end, 2.0);
    IdeogramLayout {
        segments,
        centromere,
        visible_x,
        visible_width,
        has_cytobands,
    }
}

fn ideogram_centromere(
    segments: &[IdeogramSegment],
    axis: ContigAxis,
) -> Option<IdeogramCentromere> {
    let mut best = None::<(usize, usize)>;
    let mut run_start = None;
    for (index, segment) in segments.iter().enumerate() {
        if segment.stain == CytobandStain::Centromere {
            run_start.get_or_insert(index);
        } else if let Some(start) = run_start.take()
            && best.is_none_or(|(best_start, best_end)| {
                segments[index - 1]
                    .end
                    .saturating_sub(segments[start].start)
                    > segments[best_end]
                        .end
                        .saturating_sub(segments[best_start].start)
            })
        {
            best = Some((start, index - 1));
        }
    }
    if let Some(start) = run_start {
        let end = segments.len() - 1;
        if best.is_none_or(|(best_start, best_end)| {
            segments[end].end.saturating_sub(segments[start].start)
                > segments[best_end]
                    .end
                    .saturating_sub(segments[best_start].start)
        }) {
            best = Some((start, end));
        }
    }
    let (first, last) = best?;
    let start = segments[first].start;
    let end = segments[last].end;
    let waist = if first < last {
        segments[first + (last - first + 1) / 2 - 1].end
    } else {
        start + end.saturating_sub(start) / 2
    };
    let (x, width) = axis.interval_geometry(start, end, 1.0);
    Some(IdeogramCentromere {
        x,
        width,
        waist_x: axis.x_at_boundary(waist),
        start,
        end,
    })
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
        let layout = layout_ideogram(
            &assembly,
            contig,
            visible,
            ContigAxis::new(1_000, 0.0, 400.0),
        );
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
            GenomicInterval::new(contig, 0, 100).unwrap(),
            ContigAxis::new(1_000, 0.0, 400.0),
        );
        assert!(layout.has_cytobands);
        assert!((layout.segments[0].width - 200.0).abs() < f32::EPSILON);
    }

    #[test]
    fn contig_axis_is_shared_by_bands_visible_interval_and_pointer_mapping() {
        let axis = ContigAxis::new(10_000, 100.0, 1_000.0);
        assert_eq!(axis.x_at_boundary(0), 100.0);
        assert_eq!(axis.x_at_boundary(5_000), 600.0);
        assert_eq!(axis.x_at_boundary(10_000), 1_100.0);
        assert_eq!(axis.position_at_x(100.0), 0);
        assert_eq!(axis.position_at_x(600.0), 5_000);
        assert_eq!(axis.position_at_x(1_100.0), 9_999);
        assert_eq!(axis.interval_geometry(2_500, 5_000, 2.0), (350.0, 250.0));
    }

    #[test]
    fn ideogram_derives_a_typed_centromere_from_the_acen_pair() {
        let contig = ContigId::new(0);
        let mut assembly = GenomeAssembly::from_dictionary(
            "test",
            ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000)]),
        );
        for (start, end, name, stain) in [
            (0, 400, "p1", CytobandStain::Negative),
            (400, 480, "p11", CytobandStain::Centromere),
            (480, 560, "q11", CytobandStain::Centromere),
            (560, 1_000, "q1", CytobandStain::Positive50),
        ] {
            assembly.cytobands.push(Cytoband {
                interval: GenomicInterval::new(contig, start, end).unwrap(),
                name: name.to_owned(),
                stain,
            });
        }
        let layout = layout_ideogram(
            &assembly,
            contig,
            GenomicInterval::new(contig, 100, 200).unwrap(),
            ContigAxis::new(1_000, 20.0, 500.0),
        );
        assert_eq!(
            layout.centromere,
            Some(IdeogramCentromere {
                x: 220.0,
                width: 80.0,
                waist_x: 260.0,
                start: 400,
                end: 560,
            })
        );
    }

    #[test]
    fn genomic_positions_use_grouped_decimal_digits() {
        assert_eq!(format_genomic_position(1), "1");
        assert_eq!(format_genomic_position(12_345_678), "12,345,678");
    }
}
