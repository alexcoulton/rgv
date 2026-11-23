//! Bounded CPU layout used only for standalone Sashimi exports.
//!
//! The interactive viewer consumes scene-native junction pages and never consults this module for
//! pixels or hit testing.

use std::{cmp::Reverse, collections::BTreeSet};

use crate::{
    DisplayList, RectPrimitive, RectShape, Rgba, TextPrimitive, junction_arc_stroke_width,
};
use rgv_core::{
    JunctionDisplayMode, LibraryStrandedness, SashimiCountMode, SashimiLabelMode, Viewport,
};
use rgv_model::{
    JunctionEvidence, JunctionStrand, JunctionSummary, SashimiPresentationOptions,
    SashimiPresentationTable, SpliceMotif, place_labels_by_priority, psi_label, sashimi_arc_height,
    sashimi_available_height, sashimi_stroke_width,
};

pub const MAX_ANNOTATED_JUNCTIONS: usize = 100_000;
pub const MAX_RENDERED_JUNCTIONS: usize = 4_096;
pub const MAX_RENDERED_JUNCTION_LABELS: usize = 512;
const MAX_EXPANDED_JUNCTION_ROWS: usize = 12;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum JunctionAnnotationStatus {
    Annotated,
    Novel,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JunctionAnnotationLookup {
    annotated: BTreeSet<(u64, u64)>,
    pub source_tracks: usize,
    pub complete_for_interval: bool,
    pub truncated: bool,
}

impl JunctionAnnotationLookup {
    #[must_use]
    pub fn new(
        junctions: impl IntoIterator<Item = (u64, u64)>,
        source_tracks: usize,
        complete_for_interval: bool,
    ) -> Self {
        let mut annotated = BTreeSet::new();
        let mut truncated = false;
        for junction in junctions {
            if annotated.len() >= MAX_ANNOTATED_JUNCTIONS && !annotated.contains(&junction) {
                truncated = true;
                continue;
            }
            annotated.insert(junction);
        }
        Self {
            annotated,
            source_tracks,
            complete_for_interval: complete_for_interval && !truncated && source_tracks > 0,
            truncated,
        }
    }

    #[must_use]
    pub fn status(&self, start: u64, end: u64) -> JunctionAnnotationStatus {
        if self.annotated.contains(&(start, end)) {
            JunctionAnnotationStatus::Annotated
        } else if self.complete_for_interval {
            JunctionAnnotationStatus::Novel
        } else {
            JunctionAnnotationStatus::Unknown
        }
    }

    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        self.annotated
            .iter()
            .flat_map(|(start, end)| start.to_le_bytes().into_iter().chain(end.to_le_bytes()))
            .chain(self.source_tracks.to_le_bytes())
            .chain([
                u8::from(self.complete_for_interval),
                u8::from(self.truncated),
            ])
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JunctionArcComponent {
    Positive,
    Negative,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SashimiLayoutOptions {
    pub top: f32,
    pub height: f32,
    /// Absolute weak-junction threshold.
    pub minimum_depth: u32,
    /// Relative weak-junction threshold, in percent of the strongest junction at a shared site.
    pub relative_threshold_percent: f32,
    /// Ignore both weak-junction thresholds.
    pub show_all: bool,
    pub library: LibraryStrandedness,
    pub display_mode: JunctionDisplayMode,
    /// Fixed or shared maximum in the selected count units. `None` uses this track's maximum.
    pub scale_maximum: Option<f64>,
    pub count_mode: SashimiCountMode,
    pub show_labels: bool,
    pub label_mode: SashimiLabelMode,
}

impl Default for SashimiLayoutOptions {
    fn default() -> Self {
        Self {
            top: 0.0,
            height: 90.0,
            minimum_depth: 1,
            relative_threshold_percent: 0.0,
            show_all: false,
            library: LibraryStrandedness::Unstranded,
            display_mode: JunctionDisplayMode::Collapsed,
            scale_maximum: None,
            count_mode: SashimiCountMode::Raw,
            show_labels: true,
            label_mode: SashimiLabelMode::Counts,
        }
    }
}

impl SashimiLayoutOptions {
    /// The presentation options these layout options describe (no site or transcript filter).
    #[must_use]
    pub fn presentation_options(&self) -> SashimiPresentationOptions {
        SashimiPresentationOptions {
            minimum_depth: self.minimum_depth.max(1),
            relative_threshold_percent: self.relative_threshold_percent,
            show_all: self.show_all,
            library: self.library,
            site_filter: None,
            intron_filter: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SashimiLayoutStats {
    pub rendered_junctions: usize,
    pub hidden_junctions: usize,
    pub maximum_junction_depth: u32,
    pub maximum_junction_value: f64,
    pub normalization_available: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SashimiLayout {
    pub display_list: DisplayList,
    pub stats: SashimiLayoutStats,
}

/// [`layout_sashimi_with_presentation`] with strands from tags and library only (no reference
/// motifs) and the weak-junction thresholds of `options`.
#[must_use]
pub fn layout_sashimi(
    summary: &JunctionSummary,
    viewport: Viewport,
    options: SashimiLayoutOptions,
    annotations: Option<&JunctionAnnotationLookup>,
) -> SashimiLayout {
    layout_sashimi_with_presentation(summary, viewport, options, annotations, None)
}

/// Lays out one Sashimi lane as the interactive renderer draws it: one arc per junction on its
/// resolved strand (`+` above the baseline in red, `-` below in blue, unknown above in grey),
/// rising with its on-screen span, weak junctions hidden, and count (or PSI) labels on chips
/// where the stronger junction keeps its label on collision. `presentation` supplies resolved
/// strands (with reference motifs) and filters; without one it is built from `options`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn layout_sashimi_with_presentation(
    summary: &JunctionSummary,
    viewport: Viewport,
    options: SashimiLayoutOptions,
    annotations: Option<&JunctionAnnotationLookup>,
    presentation: Option<&SashimiPresentationTable>,
) -> SashimiLayout {
    let built;
    let presentation = if let Some(presentation) = presentation {
        presentation
    } else {
        built = SashimiPresentationTable::build(
            &summary.junctions,
            |_, _| SpliceMotif::Unavailable,
            &options.presentation_options(),
        );
        &built
    };
    let interval = viewport.interval();
    let candidate_count = summary
        .junctions
        .iter()
        .filter(|junction| junction.start < interval.end() && junction.end > interval.start())
        .count();
    let mut visible = summary
        .junctions
        .iter()
        .enumerate()
        .filter(|(_, junction)| {
            junction.start < interval.end()
                && junction.end > interval.start()
                && !presentation.resolve(junction).hidden()
        })
        .collect::<Vec<_>>();
    if visible.len() > MAX_RENDERED_JUNCTIONS {
        visible.sort_by_key(|(index, junction)| {
            (
                Reverse(junction.depth),
                junction.start,
                junction.end,
                *index,
            )
        });
        visible.truncate(MAX_RENDERED_JUNCTIONS);
    }
    visible.sort_by_key(|(index, junction)| (junction.start, junction.end, *index));

    let normalization_available = options.count_mode == SashimiCountMode::Raw
        || summary.provenance.normalization_mapped_records.is_some();
    let automatic_maximum = visible
        .iter()
        .map(|(_, junction)| junction_count_value(summary, junction, options.count_mode))
        .fold(0.0_f64, f64::max);
    let scale_maximum = options
        .scale_maximum
        .filter(|maximum| maximum.is_finite() && *maximum > 0.0)
        .unwrap_or(automatic_maximum.max(f64::EPSILON));
    let rows = if options.display_mode == JunctionDisplayMode::Expanded {
        expanded_junction_rows(&visible)
    } else {
        vec![0; visible.len()]
    };
    let row_count = rows.iter().copied().max().unwrap_or(0) + 1;
    let width = viewport.pixel_width() as f32;
    let band_height = options.height / row_count.max(1) as f32;
    let labels = options.show_labels && options.label_mode != SashimiLabelMode::None;
    let mut display = DisplayList::default();
    for row in 0..row_count {
        display.rectangles.push(RectPrimitive {
            x: 0.0,
            y: options.top + (row as f32 + 0.5) * band_height,
            width,
            height: 1.0,
            color: Rgba::new(0.36, 0.38, 0.42, 0.24),
            object_id: 0,
            shape: RectShape::Rectangle,
        });
    }

    let mut label_candidates = Vec::<(TextPrimitive, [f64; 4], u64)>::new();
    for ((_, junction), row) in visible.iter().zip(rows) {
        let full_left = viewport.pixel_at_genomic(junction.start) as f32;
        let full_right = viewport.pixel_at_genomic(junction.end) as f32;
        let left = full_left.clamp(0.0, width);
        let right = full_right.clamp(0.0, width);
        if right <= left {
            continue;
        }
        let resolved = presentation.resolve(junction);
        let band_top = options.top + row as f32 * band_height;
        let band_bottom = band_top + band_height;
        let baseline = band_top + band_height * 0.5;
        let value = junction_count_value(summary, junction, options.count_mode);
        let stroke = sashimi_stroke_width(value, scale_maximum);
        let available = sashimi_available_height(f64::from(band_height), labels);
        let arc_height = sashimi_arc_height(
            f64::from((full_right - full_left).max(1.0)),
            available,
            stroke,
        ) as f32;
        let (arc_shape, color, direction_shape) = match resolved.strand {
            JunctionStrand::Positive => (
                RectShape::JunctionArcUp,
                Rgba::new(0.84, 0.30, 0.38, 0.86),
                Some(RectShape::ChevronRight),
            ),
            JunctionStrand::Negative => (
                RectShape::JunctionArcDown,
                Rgba::new(0.25, 0.45, 0.82, 0.86),
                Some(RectShape::ChevronLeft),
            ),
            JunctionStrand::Unknown => (
                RectShape::JunctionArcUp,
                Rgba::new(0.42, 0.45, 0.50, 0.80),
                None,
            ),
        };
        let arc_top = if arc_shape == RectShape::JunctionArcUp {
            baseline - arc_height
        } else {
            baseline
        };
        // The export keeps the arc on its full span (clipped by the page), so an end beyond the
        // view stays beyond it.
        display.rectangles.push(RectPrimitive {
            x: full_left,
            y: arc_top,
            width: (full_right - full_left).max(1.0),
            height: arc_height,
            color,
            object_id: 0,
            shape: arc_shape,
        });
        if let Some(direction_shape) = direction_shape {
            add_direction_marker(
                &mut display,
                full_left,
                full_right,
                arc_top,
                arc_height,
                arc_shape,
                direction_shape,
                color,
            );
        }
        if let Some(annotations) = annotations {
            let color = match annotations.status(junction.start, junction.end) {
                JunctionAnnotationStatus::Annotated => Rgba::new(0.16, 0.58, 0.28, 1.0),
                JunctionAnnotationStatus::Novel => Rgba::new(0.92, 0.48, 0.10, 1.0),
                JunctionAnnotationStatus::Unknown => Rgba::new(0.46, 0.48, 0.52, 0.92),
            };
            display.rectangles.push(RectPrimitive {
                x: ((left + right) * 0.5 - 2.5).clamp(0.0, (width - 5.0).max(0.0)),
                y: baseline - 2.5,
                width: 5.0,
                height: 5.0,
                color,
                object_id: 0,
                shape: RectShape::Rectangle,
            });
        }
        if labels && right - left >= 18.0 && band_height >= 28.0 {
            let label = match options.label_mode {
                SashimiLabelMode::Psi => psi_label(&resolved, junction.depth),
                SashimiLabelMode::Counts | SashimiLabelMode::None => {
                    junction_count_label(summary, junction, options.count_mode)
                }
            };
            let label_width = label.chars().count() as f32 * 6.0 + 6.0;
            let x = (left + right) * 0.5;
            let fraction = if full_right > full_left {
                (x - full_left) / (full_right - full_left)
            } else {
                0.5
            };
            let parabola = 4.0 * (fraction - 0.5).powi(2);
            let curve = stroke as f32 * 0.5 + (arc_height - stroke as f32).max(0.0) * parabola;
            let offset = stroke as f32 * 0.5 + 6.5;
            let label_y = if arc_shape == RectShape::JunctionArcUp {
                arc_top + curve - offset
            } else {
                arc_top + arc_height - curve + offset
            }
            .clamp(band_top + 6.0, band_bottom - 6.0);
            let label_left = (x - label_width * 0.5).clamp(0.0, (width - label_width).max(0.0));
            label_candidates.push((
                TextPrimitive {
                    x: label_left + label_width * 0.5,
                    y: label_y,
                    text: label,
                    font_size: 10.0,
                    color: Rgba::new(0.10, 0.10, 0.12, 0.94),
                },
                [
                    f64::from(label_left),
                    f64::from(label_y - 6.0),
                    f64::from(label_left + label_width),
                    f64::from(label_y + 6.0),
                ],
                u64::from(junction.depth),
            ));
        }
    }
    let boxes = label_candidates
        .iter()
        .map(|(_, bounds, depth)| (*bounds, *depth))
        .collect::<Vec<_>>();
    let mut kept = place_labels_by_priority(&boxes, 1.0);
    if kept.len() > MAX_RENDERED_JUNCTION_LABELS {
        kept.sort_by_key(|index| Reverse(boxes[*index].1));
        kept.truncate(MAX_RENDERED_JUNCTION_LABELS);
        kept.sort_unstable();
    }
    for index in kept {
        let (text, bounds, _) = &label_candidates[index];
        display.rectangles.push(RectPrimitive {
            x: bounds[0] as f32,
            y: bounds[1] as f32,
            width: (bounds[2] - bounds[0]) as f32,
            height: (bounds[3] - bounds[1]) as f32,
            color: Rgba::new(1.0, 1.0, 1.0, 0.88),
            object_id: 0,
            shape: RectShape::Rectangle,
        });
        display.texts.push(text.clone());
    }

    SashimiLayout {
        display_list: display,
        stats: SashimiLayoutStats {
            rendered_junctions: visible.len(),
            hidden_junctions: candidate_count.saturating_sub(visible.len()),
            maximum_junction_depth: summary.maximum_depth(),
            maximum_junction_value: automatic_maximum,
            normalization_available,
        },
    }
}

#[must_use]
pub fn junction_count_value(
    summary: &JunctionSummary,
    junction: &JunctionEvidence,
    mode: SashimiCountMode,
) -> f64 {
    match mode {
        SashimiCountMode::Raw => f64::from(junction.depth),
        SashimiCountMode::CountsPerMillion => summary
            .provenance
            .normalization_mapped_records
            .filter(|total| *total > 0)
            .map_or_else(
                || f64::from(junction.depth),
                |total| f64::from(junction.depth) * 1_000_000.0 / total as f64,
            ),
    }
}

fn junction_count_label(
    summary: &JunctionSummary,
    junction: &JunctionEvidence,
    mode: SashimiCountMode,
) -> String {
    match (mode, summary.provenance.normalization_mapped_records) {
        (SashimiCountMode::CountsPerMillion, Some(total)) if total > 0 => {
            let value = junction_count_value(summary, junction, mode);
            if value < 10.0 {
                format!("{value:.1} CPM")
            } else {
                format!("{value:.0} CPM")
            }
        }
        (SashimiCountMode::CountsPerMillion, _) => format!("{} raw", junction.depth),
        (SashimiCountMode::Raw, _) => junction.depth.to_string(),
    }
}

fn expanded_junction_rows(visible: &[(usize, &JunctionEvidence)]) -> Vec<usize> {
    let mut row_ends = Vec::<u64>::new();
    visible
        .iter()
        .map(|(_, junction)| {
            if let Some((row, end)) = row_ends
                .iter_mut()
                .enumerate()
                .find(|(_, end)| **end <= junction.start)
            {
                *end = junction.end;
                row
            } else if row_ends.len() < MAX_EXPANDED_JUNCTION_ROWS {
                row_ends.push(junction.end);
                row_ends.len() - 1
            } else {
                let row = row_ends
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, end)| **end)
                    .map_or(0, |(row, _)| row);
                row_ends[row] = row_ends[row].max(junction.end);
                row
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn add_direction_marker(
    display: &mut DisplayList,
    left: f32,
    right: f32,
    arc_top: f32,
    arc_height: f32,
    arc_shape: RectShape,
    direction_shape: RectShape,
    color: Rgba,
) {
    if right - left < 12.0 {
        return;
    }
    let fraction: f32 = if direction_shape == RectShape::ChevronLeft {
        0.30
    } else {
        0.70
    };
    let centre_x = left + (right - left) * fraction;
    let stroke = junction_arc_stroke_width(arc_height);
    let inner_height = (arc_height - stroke).max(0.0);
    let parabola = 4.0 * (fraction - 0.5).powi(2);
    let centre_y = if arc_shape == RectShape::JunctionArcUp {
        arc_top + stroke * 0.5 + inner_height * parabola
    } else {
        arc_top + arc_height - stroke * 0.5 - inner_height * parabola
    };
    display.rectangles.push(RectPrimitive {
        x: centre_x - 2.5,
        y: centre_y - 2.5,
        width: 5.0,
        height: 5.0,
        color,
        object_id: 0,
        shape: direction_shape,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::{ContigId, GenomicInterval};

    #[test]
    fn export_layout_draws_one_arc_per_junction_on_its_resolved_strand() {
        let interval = GenomicInterval::new(ContigId::new(1), 100, 500).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        // Untagged mates in both orientations: no arc may be drawn below the baseline.
        let summary = JunctionSummary {
            junctions: vec![
                JunctionEvidence {
                    start: 180,
                    end: 360,
                    depth: 10,
                    unknown_depth: 10,
                    fallback_positive_depth: 6,
                    fallback_negative_depth: 4,
                    ..JunctionEvidence::default()
                },
                JunctionEvidence {
                    start: 200,
                    end: 240,
                    depth: 4,
                    tagged_negative_depth: 4,
                    negative_depth: 4,
                    ..JunctionEvidence::default()
                },
            ],
            ..JunctionSummary::default()
        };
        let layout = layout_sashimi(&summary, viewport, SashimiLayoutOptions::default(), None);
        let arcs = layout
            .display_list
            .rectangles
            .iter()
            .filter(|primitive| primitive.shape.is_junction_arc())
            .collect::<Vec<_>>();
        assert_eq!(arcs.len(), 2);
        let long = arcs.iter().find(|arc| arc.width > 300.0).unwrap();
        let short = arcs.iter().find(|arc| arc.width < 100.0).unwrap();
        assert_eq!(long.shape, RectShape::JunctionArcUp);
        assert_eq!(short.shape, RectShape::JunctionArcDown);
        // Height follows span: the long intron rises higher than the short one.
        assert!(long.height > short.height);
    }

    /// Regression modelled on TM4SF1 in an ENCODE paired-end RNA-seq BAM without `XS` tags:
    /// every junction's untagged reads split about 50/50 between alignment orientations, which
    /// used to draw each junction twice (a red arc above and a blue arc below).
    #[test]
    fn untagged_paired_end_junctions_draw_once_each_and_weak_ones_hide() {
        let interval = GenomicInterval::new(ContigId::new(2), 149_360_523, 149_388_249).unwrap();
        let viewport = Viewport::new(interval, 1_200.0).unwrap();
        let untagged = |start: u64, end: u64, depth: u32| JunctionEvidence {
            start,
            end,
            depth,
            unknown_depth: depth,
            fallback_positive_depth: depth.div_ceil(2),
            fallback_negative_depth: depth / 2,
            first_of_pair_forward_depth: depth,
            ..JunctionEvidence::default()
        };
        let base = 149_362_000_u64;
        let mut junctions = vec![
            untagged(base, base + 4_000, 21_757),
            untagged(base + 4_100, base + 8_000, 20_341),
            untagged(base + 8_100, base + 12_000, 14_687),
            untagged(base + 12_100, base + 16_000, 12_300),
        ];
        // Single-read junctions: 11 sharing a strong junction's donor, 10 elsewhere.
        for index in 0..11 {
            junctions.push(untagged(base, base + 5_000 + index * 37, 1));
        }
        for index in 0..10 {
            junctions.push(untagged(
                base + 17_000 + index * 300,
                base + 17_100 + index * 300,
                1,
            ));
        }
        // Two-read junctions sharing the strongest junction's acceptor: far below 1 % of it.
        for index in 0..7 {
            junctions.push(untagged(base + 1_000 + index * 13, base + 4_000, 2));
        }
        let summary = JunctionSummary {
            junctions,
            ..JunctionSummary::default()
        };
        let options = SashimiLayoutOptions {
            minimum_depth: 2,
            relative_threshold_percent: 1.0,
            ..SashimiLayoutOptions::default()
        };
        let arcs = |layout: &SashimiLayout| {
            layout
                .display_list
                .rectangles
                .iter()
                .filter(|primitive| primitive.shape.is_junction_arc())
                .map(|primitive| primitive.shape)
                .collect::<Vec<_>>()
        };
        // Without a reference motif the strand is unknown: one grey arc above per junction.
        let layout = layout_sashimi(&summary, viewport, options, None);
        assert_eq!(layout.stats.rendered_junctions, 4);
        assert_eq!(layout.stats.hidden_junctions, 28);
        assert_eq!(arcs(&layout), vec![RectShape::JunctionArcUp; 4]);
        // With the reference's CT..AC motif (TM4SF1 is a minus-strand gene) every arc is drawn
        // once, below the baseline.
        let presentation = SashimiPresentationTable::build(
            &summary.junctions,
            |_, _| SpliceMotif::CtAc,
            &options.presentation_options(),
        );
        let layout = layout_sashimi_with_presentation(
            &summary,
            viewport,
            options,
            None,
            Some(&presentation),
        );
        assert_eq!(arcs(&layout), vec![RectShape::JunctionArcDown; 4]);
        // Showing all junctions draws all 32, still once each.
        let all = SashimiLayoutOptions {
            show_all: true,
            ..options
        };
        let layout = layout_sashimi(&summary, viewport, all, None);
        assert_eq!(arcs(&layout).len(), 32);
    }

    #[test]
    fn bounded_export_layout_preserves_components_labels_and_annotation() {
        let interval = GenomicInterval::new(ContigId::new(1), 100, 500).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        let summary = JunctionSummary {
            junctions: vec![JunctionEvidence {
                start: 180,
                end: 360,
                depth: 10,
                tagged_positive_depth: 6,
                tagged_negative_depth: 4,
                positive_depth: 6,
                negative_depth: 4,
                ..JunctionEvidence::default()
            }],
            ..JunctionSummary::default()
        };
        let annotations = JunctionAnnotationLookup::new([(180, 360)], 1, true);
        let layout = layout_sashimi(
            &summary,
            viewport,
            SashimiLayoutOptions::default(),
            Some(&annotations),
        );
        assert_eq!(layout.stats.rendered_junctions, 1);
        // One arc, on the tagged majority strand; the minority strand is not drawn separately.
        assert_eq!(
            layout
                .display_list
                .rectangles
                .iter()
                .filter(|primitive| primitive.shape.is_junction_arc())
                .map(|primitive| primitive.shape)
                .collect::<Vec<_>>(),
            vec![RectShape::JunctionArcUp]
        );
        // The annotation marker is kept.
        assert!(layout.display_list.rectangles.iter().any(|primitive| {
            primitive.shape == RectShape::Rectangle
                && primitive.color == Rgba::new(0.16, 0.58, 0.28, 1.0)
        }));
        assert!(!layout.display_list.texts.is_empty());
    }
}
