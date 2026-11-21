//! Bounded CPU layout used only for standalone Sashimi exports.
//!
//! The interactive viewer consumes scene-native junction pages and never consults this module for
//! pixels or hit testing.

use std::{cmp::Reverse, collections::BTreeSet};

use crate::{
    DisplayList, RectPrimitive, RectShape, Rgba, TextPrimitive, junction_arc_stroke_width,
};
use rgv_core::{JunctionDisplayMode, SashimiCountMode, Viewport};
use rgv_model::{JunctionEvidence, JunctionSummary};

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
    pub minimum_depth: u32,
    pub display_mode: JunctionDisplayMode,
    /// Fixed or shared maximum in the selected count units. `None` uses this track's maximum.
    pub scale_maximum: Option<f64>,
    pub count_mode: SashimiCountMode,
    pub show_labels: bool,
}

impl Default for SashimiLayoutOptions {
    fn default() -> Self {
        Self {
            top: 0.0,
            height: 90.0,
            minimum_depth: 1,
            display_mode: JunctionDisplayMode::Collapsed,
            scale_maximum: None,
            count_mode: SashimiCountMode::Raw,
            show_labels: true,
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

#[must_use]
pub fn layout_sashimi(
    summary: &JunctionSummary,
    viewport: Viewport,
    options: SashimiLayoutOptions,
    annotations: Option<&JunctionAnnotationLookup>,
) -> SashimiLayout {
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
                && junction.depth >= options.minimum_depth
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

    let mut rendered_labels = 0_usize;
    let mut occupied_label_bounds = Vec::<(f32, f32, f32, f32)>::new();
    for ((_, junction), row) in visible.iter().zip(rows) {
        let left = (viewport.pixel_at_genomic(junction.start) as f32).clamp(0.0, width);
        let right = (viewport.pixel_at_genomic(junction.end) as f32).clamp(0.0, width);
        if right <= left {
            continue;
        }
        let band_top = options.top + row as f32 * band_height;
        let band_bottom = band_top + band_height;
        let baseline = band_top + band_height * 0.5;
        let apex_clearance = if options.show_labels { 11.0 } else { 1.5 };
        let available_height = (band_height * 0.5 - apex_clearance).max(3.0);
        let components = [
            (
                junction.positive_depth,
                RectShape::JunctionArcUp,
                Rgba::new(0.84, 0.30, 0.38, 0.82),
                Some(RectShape::ChevronRight),
            ),
            (
                junction.negative_depth,
                RectShape::JunctionArcDown,
                Rgba::new(0.25, 0.45, 0.82, 0.82),
                Some(RectShape::ChevronLeft),
            ),
            (
                junction.unknown_depth,
                RectShape::JunctionArcUp,
                Rgba::new(0.42, 0.45, 0.50, 0.76),
                None,
            ),
        ];
        let mut component_apices = Vec::with_capacity(3);
        for (depth, arc_shape, color, direction_shape) in components {
            if depth == 0 {
                continue;
            }
            let value = junction_component_value(summary, junction, depth, options.count_mode);
            let fraction = (value / scale_maximum).clamp(0.0, 1.0).sqrt() as f32;
            let arc_height =
                (available_height * (0.18 + 0.82 * fraction)).clamp(3.0, available_height);
            let arc_top = if arc_shape == RectShape::JunctionArcUp {
                baseline - arc_height
            } else {
                baseline
            };
            display.rectangles.push(RectPrimitive {
                x: left,
                y: arc_top,
                width: right - left,
                height: arc_height,
                color,
                object_id: 0,
                shape: arc_shape,
            });
            if let Some(direction_shape) = direction_shape {
                add_direction_marker(
                    &mut display,
                    left,
                    right,
                    arc_top,
                    arc_height,
                    arc_shape,
                    direction_shape,
                    color,
                );
            }
            let stroke = junction_arc_stroke_width(arc_height);
            let apex = if arc_shape == RectShape::JunctionArcUp {
                arc_top + stroke * 0.5
            } else {
                arc_top + arc_height - stroke * 0.5
            };
            component_apices.push((depth, arc_shape, apex));
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
        if options.show_labels
            && rendered_labels < MAX_RENDERED_JUNCTION_LABELS
            && right - left >= 30.0
            && band_height >= 28.0
        {
            let label = junction_count_label(summary, junction, options.count_mode);
            let label_width = (label.chars().count() as f32 * 6.2 + 2.0).clamp(10.0, right - left);
            let label_y = component_apices
                .iter()
                .max_by_key(|(depth, _, _)| *depth)
                .map_or(baseline - 7.0, |(_, shape, apex)| {
                    if *shape == RectShape::JunctionArcUp {
                        *apex - 7.0
                    } else {
                        *apex + 7.0
                    }
                })
                .clamp(band_top + 5.5, band_bottom - 5.5);
            let label_left =
                ((left + right - label_width) * 0.5).clamp(0.0, (width - label_width).max(0.0));
            let bounds = (
                label_left,
                label_y - 5.5,
                label_left + label_width,
                label_y + 5.5,
            );
            if occupied_label_bounds.iter().any(|other| {
                bounds.0 < other.2 + 2.0
                    && bounds.2 > other.0 - 2.0
                    && bounds.1 < other.3 + 2.0
                    && bounds.3 > other.1 - 2.0
            }) {
                continue;
            }
            occupied_label_bounds.push(bounds);
            display.texts.push(TextPrimitive {
                x: (left + right) * 0.5,
                y: label_y,
                text: label,
                font_size: 10.0,
                color: Rgba::new(0.10, 0.10, 0.12, 0.94),
            });
            rendered_labels = rendered_labels.saturating_add(1);
        }
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

fn junction_component_value(
    summary: &JunctionSummary,
    junction: &JunctionEvidence,
    component_depth: u32,
    mode: SashimiCountMode,
) -> f64 {
    if junction.depth == 0 {
        return 0.0;
    }
    junction_count_value(summary, junction, mode) * f64::from(component_depth)
        / f64::from(junction.depth)
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
    fn bounded_export_layout_preserves_components_labels_and_annotation() {
        let interval = GenomicInterval::new(ContigId::new(1), 100, 500).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        let summary = JunctionSummary {
            junctions: vec![JunctionEvidence {
                start: 180,
                end: 360,
                depth: 10,
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
        assert!(
            layout
                .display_list
                .rectangles
                .iter()
                .any(|primitive| primitive.shape == RectShape::JunctionArcUp)
        );
        assert!(
            layout
                .display_list
                .rectangles
                .iter()
                .any(|primitive| primitive.shape == RectShape::JunctionArcDown)
        );
        assert!(!layout.display_list.texts.is_empty());
    }
}
