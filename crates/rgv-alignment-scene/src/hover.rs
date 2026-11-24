//! Pointer hover over the read stack: which read event (or read) is under the pointer, found on
//! the exact geometry the frame draws, and the outline its highlight follows.
//!
//! Hover is resolved against the same captured camera and scene as rendering and picking
//! ([`CpuAlignmentProjector::pick_panel_lane`]), through each page's spatial index, so its cost
//! follows the few primitives near the pointer and never the length of the row (an ONT read
//! can put hundreds of thousands of primitives into one page).
//!
//! Hover differs from a click in one way only: a mark narrower than
//! [`HOVER_MINIMUM_TARGET_WIDTH`] points (a hairline insertion, a one-point substitution when
//! zoomed out, an I-beam's bar) answers to a band that wide centred on it, so it can be found
//! with the pointer. The drawing is unchanged. An event the pointer is exactly over always wins;
//! an enlarged target wins only over a read body beneath it, never over a mark painted above.

use std::ops::Range;

use rgv_core::{GenomicInterval, TrackId};

use crate::cpu_projector::{
    MINIMUM_PRIMITIVE_EXTENT, POINT_MARKER_MAXIMUM_WIDTH, SOFT_CLIP_EDGE_MAXIMUM_WIDTH,
    for_each_row_window_primitive, insertion_serif_thickness, project_primitive,
};
use crate::semantic_page::primitive_paint_layer;
use crate::{
    AlignmentCamera, AlignmentComponent, AlignmentHandle, AlignmentHit, AlignmentInteractionToken,
    AlignmentPanelLaneFrame, AlignmentPresentationScene, AlignmentSemanticPrimitive,
    CpuAlignmentProjector, CpuPrimitiveShape, CpuProjectedPrimitive,
};

/// Narrowest hover target, in points, of any read event: a narrower mark answers to a band this
/// wide centred on it.
pub const HOVER_MINIMUM_TARGET_WIDTH: f64 = 6.0;

/// The read event (or read) under the pointer, in camera-independent terms, so its highlight can
/// be projected again through whichever camera the presented frame carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentHoverTarget {
    pub hit: AlignmentHit,
    /// The hovered primitive with absolute genomic offsets (`start_offset..end_offset`) and
    /// its canonical atlas row as `row_offset`.
    pub primitive: AlignmentSemanticPrimitive,
}

impl AlignmentHoverTarget {
    /// Canonical atlas row of the hovered primitive.
    #[must_use]
    pub const fn row(&self) -> u32 {
        self.primitive.row_offset
    }

    /// Absolute genomic span of the hovered primitive (for an event split across pages, the
    /// fragment under the pointer).
    #[must_use]
    pub const fn genomic_span(&self) -> Range<u64> {
        self.primitive.start_offset..self.primitive.end_offset
    }

    /// Whether the pointer is over an event of the read rather than the read itself.
    #[must_use]
    pub const fn is_event(&self) -> bool {
        is_hover_event(self.hit.component)
    }

    /// The hovered primitive as `camera` draws it, or `None` when it is off the pane or not
    /// drawn at this zoom.
    #[must_use]
    pub fn project(&self, camera: AlignmentCamera) -> Option<CpuProjectedPrimitive> {
        project_primitive(
            self.primitive,
            0,
            self.row(),
            camera,
            None,
            AlignmentInteractionToken::default(),
        )
    }
}

/// A hover probe's answer and the number of primitives it projected to find it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentHoverProbe {
    pub target: Option<AlignmentHoverTarget>,
    pub candidates_examined: usize,
}

/// Components that are events of a read (hovered as themselves); the rest are the read.
#[must_use]
pub const fn is_hover_event(component: AlignmentComponent) -> bool {
    !matches!(
        component,
        AlignmentComponent::ReadSpan
            | AlignmentComponent::AlignedSegment { .. }
            | AlignmentComponent::DirectionTerminal
            | AlignmentComponent::PairConnector { .. }
            | AlignmentComponent::SpecialistUnavailable { .. }
            | AlignmentComponent::Label { .. }
    )
}

/// A deletion or skip is drawn as a thin line across its gap, but the gap is the event: it is
/// hovered (and highlighted) over the read's full height.
const fn spans_read_height(primitive: &CpuProjectedPrimitive) -> bool {
    matches!(primitive.shape, CpuPrimitiveShape::EventLine)
        && matches!(
            primitive.component,
            AlignmentComponent::Deletion { .. } | AlignmentComponent::ReferenceSkip { .. }
        )
}

/// The box a hovered primitive is outlined by and found in: its drawn box, or for a deletion or
/// skip its gap at the read's full height. `(x, y, width, height)` in pane points.
#[must_use]
pub fn hover_box(
    primitive: &CpuProjectedPrimitive,
    camera: AlignmentCamera,
) -> (f64, f64, f64, f64) {
    if spans_read_height(primitive) {
        let (top, height) = camera.read_extent(primitive.row);
        (primitive.x, top, primitive.width, height)
    } else if let Some(mark) = primitive.insertion_mark {
        (mark.x, mark.y, mark.width, mark.height)
    } else {
        (primitive.x, primitive.y, primitive.width, primitive.height)
    }
}

fn exactly_under(
    primitive: &CpuProjectedPrimitive,
    camera: AlignmentCamera,
    x: f64,
    y: f64,
) -> bool {
    if spans_read_height(primitive) {
        let (left, top, width, height) = hover_box(primitive, camera);
        return x >= left && x < left + width && y >= top && y < top + height;
    }
    primitive.contains(x, y)
}

/// Horizontal distance from `x` to the centre of a mark narrower than
/// [`HOVER_MINIMUM_TARGET_WIDTH`] whose enlarged band contains `(x, y)`; `None` otherwise.
fn enlarged_distance(
    primitive: &CpuProjectedPrimitive,
    camera: AlignmentCamera,
    x: f64,
    y: f64,
) -> Option<f64> {
    let (left, top, width, height) = hover_box(primitive, camera);
    // An I-beam's box is wide, but its bar is not: the bar is what the band widens.
    let drawn_width = primitive.insertion_mark.map_or(width, |mark| {
        (mark.width - 2.0 * crate::INSERTION_SERIF_WIDTH).max(0.0)
    });
    if drawn_width >= HOVER_MINIMUM_TARGET_WIDTH || y < top || y >= top + height {
        return None;
    }
    let centre = left + width * 0.5;
    let distance = (x - centre).abs();
    (distance < HOVER_MINIMUM_TARGET_WIDTH * 0.5).then_some(distance)
}

fn paint_rank(
    primitive: &CpuProjectedPrimitive,
) -> (u8, bool, u32, AlignmentComponent, AlignmentHandle) {
    (
        primitive_paint_layer(primitive.component, primitive.geometry),
        is_hover_event(primitive.component),
        primitive.draw_order,
        primitive.component,
        primitive.handle,
    )
}

impl CpuAlignmentProjector {
    /// The read event or read under `(x, y)` in the immutable panel-lane capture the GPU draws;
    /// see the module documentation for the rules.
    #[must_use]
    pub fn hover_panel_lane(lane: &AlignmentPanelLaneFrame, x: f64, y: f64) -> AlignmentHoverProbe {
        let (Some(snapshot), Some(camera)) = (&lane.snapshot, lane.read_camera) else {
            return AlignmentHoverProbe {
                target: None,
                candidates_examined: 0,
            };
        };
        hover_captured_scene(
            snapshot.reads(),
            lane.track_id,
            camera,
            lane.covered_interval,
            lane.covered_rows.clone(),
            x,
            y,
        )
    }
}

/// The genomic window around pane `x` that holds every primitive which can be drawn, or
/// hovered, under the pointer: an I-beam's half width, a clip edge behind an arrowhead, half the
/// enlarged band and the one-point minimum size, with a base of rounding on each side.
fn hover_window(
    camera: AlignmentCamera,
    covered_interval: GenomicInterval,
    x: f64,
) -> Option<(GenomicInterval, Range<u64>)> {
    let reach_points = (POINT_MARKER_MAXIMUM_WIDTH * 0.5)
        .max(
            SOFT_CLIP_EDGE_MAXIMUM_WIDTH
                + camera.read_height().max(MINIMUM_PRIMITIVE_EXTENT) * 0.55,
        )
        .max(HOVER_MINIMUM_TARGET_WIDTH * 0.5)
        + MINIMUM_PRIMITIVE_EXTENT;
    let pointer = camera.viewport().genomic_at_pixel(x - camera.clip().x);
    let reach = reach_points * camera.bases_per_pixel();
    let window_start = ((pointer - reach).floor() - 1.0).max(0.0) as u64;
    let window_end = ((pointer + reach).floor().max(0.0) as u64).saturating_add(2);
    let start = window_start.max(covered_interval.start());
    let end = window_end.min(covered_interval.end());
    if start >= end {
        return None;
    }
    let interval = GenomicInterval::new(covered_interval.contig(), start, end).ok()?;
    Some((interval, start..end))
}

/// [`CpuAlignmentProjector::hover_panel_lane`] on an explicit scene capture.
#[must_use]
pub fn hover_captured_scene(
    scene: &AlignmentPresentationScene,
    track: TrackId,
    camera: AlignmentCamera,
    covered_interval: Option<GenomicInterval>,
    covered_rows: Range<u32>,
    x: f64,
    y: f64,
) -> AlignmentHoverProbe {
    let probe = AlignmentHoverProbe {
        target: None,
        candidates_examined: 0,
    };
    if !camera.clip().contains(x, y) {
        return probe;
    }
    let (Some(row), Some(covered_interval)) = (camera.row_for_y(y), covered_interval) else {
        return probe;
    };
    if row < covered_rows.start || row >= covered_rows.end {
        return probe;
    }
    let Some((interval, window)) = hover_window(camera, covered_interval, x) else {
        return probe;
    };
    hover_among(scene, track, camera, row, x, y, |visit| {
        for_each_row_window_primitive(scene, interval, row, window, visit);
    })
}

/// The hover rule applied to the primitives of `row` that `enumerate` hands its visitor (with
/// each one's page start); see the module documentation.
fn hover_among(
    scene: &AlignmentPresentationScene,
    track: TrackId,
    camera: AlignmentCamera,
    row: u32,
    x: f64,
    y: f64,
    enumerate: impl FnOnce(&mut dyn FnMut(u64, AlignmentSemanticPrimitive)),
) -> AlignmentHoverProbe {
    let mut candidates_examined = 0;
    let mut exact: Option<(CpuProjectedPrimitive, AlignmentSemanticPrimitive)> = None;
    let mut enlarged: Option<(f64, CpuProjectedPrimitive, AlignmentSemanticPrimitive)> = None;
    enumerate(&mut |page_start, primitive| {
        if matches!(primitive.component, AlignmentComponent::Label { .. }) {
            return;
        }
        candidates_examined += 1;
        let Some(projected) = project_primitive(
            primitive,
            page_start,
            row,
            camera,
            None,
            AlignmentInteractionToken::default(),
        ) else {
            return;
        };
        let absolute = AlignmentSemanticPrimitive {
            start_offset: page_start.saturating_add(primitive.start_offset),
            end_offset: page_start.saturating_add(primitive.end_offset),
            row_offset: row,
            ..primitive
        };
        if exactly_under(&projected, camera, x, y) {
            if exact
                .as_ref()
                .is_none_or(|(best, _)| paint_rank(&projected) > paint_rank(best))
            {
                exact = Some((projected, absolute));
            }
        } else if is_hover_event(projected.component)
            && let Some(distance) = enlarged_distance(&projected, camera, x, y)
            && enlarged.as_ref().is_none_or(|(best_distance, best, _)| {
                distance
                    .total_cmp(best_distance)
                    .then_with(|| paint_rank(best).cmp(&paint_rank(&projected)))
                    .is_lt()
            })
        {
            enlarged = Some((distance, projected, absolute));
        }
    });
    // An event exactly under the pointer wins. Otherwise a nearby small mark painted at or
    // above what is under the pointer (a read body, or nothing) takes its enlarged target.
    let exact_layer = exact
        .as_ref()
        .map(|(primitive, _)| primitive_paint_layer(primitive.component, primitive.geometry));
    let chosen = match exact {
        Some((primitive, absolute)) if is_hover_event(primitive.component) => {
            Some((primitive, absolute))
        }
        _ => enlarged
            .filter(|(_, primitive, _)| {
                exact_layer.is_none_or(|layer| {
                    primitive_paint_layer(primitive.component, primitive.geometry) >= layer
                })
            })
            .map(|(_, primitive, absolute)| (primitive, absolute))
            .or(exact),
    };
    AlignmentHoverProbe {
        target: chosen.map(|(primitive, absolute)| AlignmentHoverTarget {
            hit: AlignmentHit {
                track,
                scene_revision: scene.revision(),
                handle: primitive.handle,
                component: primitive.component,
            },
            primitive: absolute,
        }),
        candidates_examined,
    }
}

/// [`hover_captured_scene`] without the spatial index: every primitive of the row on every
/// visible page is examined. The reference the indexed hover must reproduce.
#[cfg(test)]
pub(crate) fn hover_unindexed(
    scene: &AlignmentPresentationScene,
    track: TrackId,
    camera: AlignmentCamera,
    covered_interval: Option<GenomicInterval>,
    covered_rows: Range<u32>,
    x: f64,
    y: f64,
) -> AlignmentHoverProbe {
    let none = AlignmentHoverProbe {
        target: None,
        candidates_examined: 0,
    };
    if !camera.clip().contains(x, y) {
        return none;
    }
    let (Some(row), Some(covered_interval)) = (camera.row_for_y(y), covered_interval) else {
        return none;
    };
    if row < covered_rows.start || row >= covered_rows.end {
        return none;
    }
    hover_among(scene, track, camera, row, x, y, |visit| {
        for page in scene.visible_pages(covered_interval, row..row.saturating_add(1)) {
            let page_start = page.interval().start();
            let first_row = page.row_range().start;
            for primitive in page.primitives() {
                if first_row.saturating_add(primitive.row_offset) == row {
                    visit(page_start, *primitive);
                }
            }
        }
    })
}

/// The closed outline, in pane points, that a hover highlight follows around `primitive` as
/// `camera` draws it, `gap` points clear of its painted shape on every side: for an I-beam, its
/// bar and both serifs; for anything else, its box (for a deletion or skip, the gap at the
/// read's full height; see [`hover_box`]). Vertices run clockwise on screen.
#[must_use]
pub fn hover_outline(
    primitive: &CpuProjectedPrimitive,
    camera: AlignmentCamera,
    gap: f64,
) -> Vec<[f64; 2]> {
    let (x, y, width, height) = hover_box(primitive, camera);
    if primitive.insertion_mark.is_some() {
        let serif = insertion_serif_thickness(height);
        let bar = (width - 2.0 * crate::INSERTION_SERIF_WIDTH).max(0.0);
        let (left, right, top, bottom) = (x - gap, x + width + gap, y - gap, y + height + gap);
        let (bar_left, bar_right) = (x + (width - bar) * 0.5 - gap, x + (width + bar) * 0.5 + gap);
        let (under_top, over_bottom) = (y + serif + gap, y + height - serif - gap);
        if under_top < over_bottom && bar_left > left && bar_right < right {
            return vec![
                [left, top],
                [right, top],
                [right, under_top],
                [bar_right, under_top],
                [bar_right, over_bottom],
                [right, over_bottom],
                [right, bottom],
                [left, bottom],
                [left, over_bottom],
                [bar_left, over_bottom],
                [bar_left, under_top],
                [left, under_top],
            ];
        }
    }
    vec![
        [x - gap, y - gap],
        [x + width + gap, y - gap],
        [x + width + gap, y + height + gap],
        [x - gap, y + height + gap],
    ]
}
