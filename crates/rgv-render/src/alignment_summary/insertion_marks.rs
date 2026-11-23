//! IGV-style insertion marks on the coverage lane.
//!
//! The exact allele pass already counts, per base, the eligible reads that carry an insertion
//! anchored on that base (the insertion sits on the boundary between the base and the next one).
//! Those counts arrive in the same allele pages that colour mismatch fractions, so a mark needs
//! no second pass and no extra storage: the pages only exist at per-base zoom (see
//! `MAXIMUM_ALLELE_COVERAGE_BASES_PER_PIXEL`), and only bases with an insertion are visited.
//!
//! A mark is drawn when the insertion fraction reaches the same `minimum_allele_fraction`
//! threshold that colours mismatch fractions. Its stem grows with the fraction. When more than
//! one boundary falls in a pixel column (between one and two bases per pixel), the column keeps
//! the largest fraction.

use rgv_alignment_scene::{
    AlignmentSceneSnapshot, CpuSummaryCamera, SummaryPageFamily, SummarySemanticComponent,
};
use rgv_core::GenomicInterval;
use rgv_model::PileupBin;

/// One base boundary where eligible reads carry an insertion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverageInsertionMark {
    /// Zero-based reference base the insertion follows; the mark sits on its right edge.
    pub anchor: u64,
    /// Eligible reads with an insertion at this boundary.
    pub insertions: u32,
    /// Eligible reads spanning the anchor base, the fraction's denominator.
    pub covering: u32,
}

impl CoverageInsertionMark {
    /// Insertion-bearing reads over reads spanning the boundary, in `0..=1`.
    #[must_use]
    pub fn fraction(self) -> f64 {
        if self.covering == 0 {
            return 0.0;
        }
        (f64::from(self.insertions) / f64::from(self.covering)).clamp(0.0, 1.0)
    }

    /// The mark for one per-base allele bin, if any read there carries an insertion.
    #[must_use]
    pub fn from_bin(anchor: u64, bin: &PileupBin) -> Option<Self> {
        let insertions = bin.alleles.insertion;
        (insertions > 0).then(|| Self {
            anchor,
            insertions,
            covering: insertion_covering_reads(bin),
        })
    }

    /// Whether the mark reaches the coverage track's allele-fraction threshold.
    #[must_use]
    pub fn passes(self, minimum_fraction: f64) -> bool {
        self.insertions > 0 && self.fraction() >= minimum_fraction
    }
}

/// Reads spanning an insertion's anchor base: aligned bases and deletions there, and never fewer
/// than the insertions themselves (a read can open an insertion right after a low-quality or
/// masked base).
#[must_use]
pub fn insertion_covering_reads(bin: &PileupBin) -> u32 {
    bin.depth
        .max(bin.alleles.reference_observations())
        .max(bin.alleles.insertion)
}

/// Insertion marks at or above `minimum_fraction` whose boundary lies in `covered`, from the
/// scene's per-base allele pages, in genomic order. Empty when the scene has no allele detail.
#[must_use]
pub fn coverage_insertion_marks(
    scene: &AlignmentSceneSnapshot,
    covered: GenomicInterval,
    minimum_fraction: f64,
) -> Vec<CoverageInsertionMark> {
    let mut marks = Vec::new();
    for page in scene.summary_pages_for(SummaryPageFamily::Alleles, covered) {
        let page_start = page.interval().start();
        for primitive in page.primitives() {
            let SummarySemanticComponent::Alleles { bin, .. } = primitive.component else {
                continue;
            };
            let anchor = page_start.saturating_add(primitive.start_offset);
            if !covered.contains(anchor) {
                continue;
            }
            if let Some(mark) = CoverageInsertionMark::from_bin(anchor, &bin)
                && mark.passes(minimum_fraction)
            {
                marks.push(mark);
            }
        }
    }
    marks.sort_unstable_by_key(|mark| mark.anchor);
    marks.dedup_by_key(|mark| mark.anchor);
    marks
}

/// One rectangle of a mark, in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InsertionMarkRect {
    pub origin: [f32; 2],
    pub size: [f32; 2],
}

/// Rectangles per drawn mark: a cap across the top of the lane and a stem below it.
pub const INSERTION_MARK_RECTS: usize = 2;

/// Most marks one coverage lane draws per frame. Each occupies at least one pixel column, so this
/// is never reached on a real display; it bounds the precharged overlay buffer.
pub const MAXIMUM_INSERTION_MARKS_PER_LANE: usize = 4_096;

/// Fraction of the lane height a mark at 100% reaches.
const MAXIMUM_STEM_FRACTION: f64 = 0.6;

/// The cap and stem of every visible mark. Marks whose boundary falls in the same pixel column
/// collapse to the one with the largest fraction, so a 1-2 bp/px view never stacks overdraw.
#[must_use]
pub fn insertion_mark_rects(
    camera: CpuSummaryCamera,
    marks: &[CoverageInsertionMark],
) -> Vec<InsertionMarkRect> {
    let clip = camera.clip();
    let bases_per_pixel = camera.bases_per_pixel();
    if !(bases_per_pixel.is_finite() && bases_per_pixel > 0.0) || clip.height <= 0.0 {
        return Vec::new();
    }
    let pixels_per_base = 1.0 / bases_per_pixel;
    let exact_start = camera.exact_start();
    let mut columns: Vec<(i64, f64, f64)> = Vec::with_capacity(marks.len());
    for mark in marks {
        // The boundary between the anchor base and the next one.
        let x = clip.x + (mark.anchor as f64 + 1.0 - exact_start) * pixels_per_base;
        if x < clip.x || x > clip.x + clip.width {
            continue;
        }
        let column = x.floor() as i64;
        let fraction = mark.fraction();
        match columns.last_mut() {
            Some(last) if last.0 == column => {
                if fraction > last.2 {
                    *last = (column, x, fraction);
                }
            }
            _ => columns.push((column, x, fraction)),
        }
    }
    columns.truncate(MAXIMUM_INSERTION_MARKS_PER_LANE);
    let cap_width = (pixels_per_base * 0.6).clamp(5.0, 12.0);
    let stem_width = (pixels_per_base * 0.12).clamp(2.0, 3.0);
    let cap_height = 2.0_f64.min(clip.height);
    let mut rects = Vec::with_capacity(columns.len() * INSERTION_MARK_RECTS);
    for (_, x, fraction) in columns {
        let stem_height = (clip.height * MAXIMUM_STEM_FRACTION * fraction)
            .max(4.0)
            .min(clip.height);
        let cap_left =
            (x - cap_width * 0.5).clamp(clip.x, clip.x + clip.width - cap_width.min(clip.width));
        rects.push(InsertionMarkRect {
            origin: [cap_left as f32, clip.y as f32],
            size: [cap_width.min(clip.width) as f32, cap_height as f32],
        });
        rects.push(InsertionMarkRect {
            origin: [(x - stem_width * 0.5) as f32, clip.y as f32],
            size: [stem_width as f32, stem_height as f32],
        });
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_alignment_scene::CpuClipRect;
    use rgv_core::{ContigId, Viewport};

    fn bin(depth: u32, matched: u32, insertions: u32) -> PileupBin {
        let mut bin = PileupBin {
            depth,
            ..PileupBin::default()
        };
        bin.alleles.a = matched;
        bin.alleles.insertion = insertions;
        bin
    }

    fn camera(start: u64, bases: u64, width: f64) -> CpuSummaryCamera {
        let viewport = Viewport::new(
            GenomicInterval::new(ContigId::new(0), start, start + bases).unwrap(),
            width,
        )
        .unwrap();
        CpuSummaryCamera::from_viewport(
            viewport,
            CpuClipRect {
                x: 10.0,
                y: 100.0,
                width,
                height: 50.0,
            },
        )
        .unwrap()
    }

    #[test]
    fn fraction_uses_reads_spanning_the_anchor() {
        let mark = CoverageInsertionMark::from_bin(7, &bin(40, 40, 30)).unwrap();
        assert_eq!((mark.insertions, mark.covering), (30, 40));
        assert!((mark.fraction() - 0.75).abs() < 1.0e-12);
        // Deletions span the anchor too; insertions never exceed their denominator.
        let mut deleted = bin(10, 10, 4);
        deleted.alleles.deletion = 10;
        assert_eq!(insertion_covering_reads(&deleted), 20);
        assert_eq!(insertion_covering_reads(&bin(0, 0, 3)), 3);
        assert!(CoverageInsertionMark::from_bin(7, &bin(40, 40, 0)).is_none());
    }

    #[test]
    fn threshold_matches_the_allele_fraction_rule() {
        let at = CoverageInsertionMark::from_bin(0, &bin(10, 10, 2)).unwrap();
        assert!(
            at.passes(0.2),
            "the threshold is inclusive, like mismatch colouring"
        );
        assert!(!at.passes(0.21));
        let any = CoverageInsertionMark::from_bin(0, &bin(1_000, 1_000, 1)).unwrap();
        assert!(any.passes(0.0));
        assert!(!any.passes(0.2));
    }

    #[test]
    fn marks_sit_on_the_boundary_and_scale_with_the_fraction() {
        // 40 bases over 400 points: 10 points per base.
        let camera = camera(1_000, 40, 400.0);
        let half = CoverageInsertionMark {
            anchor: 1_009,
            insertions: 5,
            covering: 10,
        };
        let full = CoverageInsertionMark {
            anchor: 1_019,
            insertions: 10,
            covering: 10,
        };
        let rects = insertion_mark_rects(camera, &[half, full]);
        assert_eq!(rects.len(), 2 * INSERTION_MARK_RECTS);
        let (cap, stem) = (rects[0], rects[1]);
        // Boundary between bases 1009 and 1010 is 10 bases in: x = 10 + 100.
        let stem_centre = stem.origin[0] + stem.size[0] * 0.5;
        assert!((stem_centre - 110.0).abs() < 1.0e-4);
        let cap_centre = cap.origin[0] + cap.size[0] * 0.5;
        assert!((cap_centre - 110.0).abs() < 1.0e-4);
        assert!((cap.origin[1] - 100.0).abs() < f32::EPSILON);
        assert!((stem.size[1] - 15.0).abs() < 1.0e-4, "50 pt × 0.6 × 0.5");
        assert!(
            (rects[3].size[1] - 30.0).abs() < 1.0e-4,
            "50 pt × 0.6 × 1.0"
        );
    }

    #[test]
    fn sub_pixel_boundaries_keep_the_largest_fraction_per_column() {
        // Two bases per pixel: the boundaries after bases 99 and 100 (x = 60.0 and 60.5) share
        // a column.
        let camera = camera(0, 800, 400.0);
        let marks = [
            CoverageInsertionMark {
                anchor: 99,
                insertions: 3,
                covering: 10,
            },
            CoverageInsertionMark {
                anchor: 100,
                insertions: 9,
                covering: 10,
            },
            CoverageInsertionMark {
                anchor: 500,
                insertions: 5,
                covering: 10,
            },
        ];
        let rects = insertion_mark_rects(camera, &marks);
        assert_eq!(rects.len(), 2 * INSERTION_MARK_RECTS);
        assert!((rects[1].size[1] - 27.0).abs() < 1.0e-4, "0.9 survives");
        // Off-screen boundaries are skipped.
        let off = CoverageInsertionMark {
            anchor: 5_000,
            insertions: 5,
            covering: 10,
        };
        assert!(insertion_mark_rects(camera, &[off]).is_empty());
    }
}
