use super::*;

pub(crate) const JUNCTION_ARC_MINIMUM_ACQUISITION_RADIUS: f32 = 4.0;
pub(crate) const JUNCTION_ARC_ACQUISITION_MARGIN: f32 = 3.0;
pub(crate) const JUNCTION_ARC_EXIT_MARGIN: f32 = 2.0;
const JUNCTION_ARC_TRANSFER_ADVANTAGE: f32 = 1.0;
const JUNCTION_ARC_DISTANCE_TIE_EPSILON: f32 = 0.05;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct JunctionArcHitTestResult {
    pub(crate) hover: Option<JunctionArcHover>,
    pub(crate) gate: SashimiHitTestGate,
    pub(crate) lane_track_id: Option<TrackId>,
    pub(crate) total_targets: usize,
    pub(crate) track_targets: usize,
    pub(crate) current_revision_targets: usize,
    pub(crate) bounds_candidates: usize,
    pub(crate) curve_evaluations: usize,
    pub(crate) stale_revision_rejections: usize,
    pub(crate) nearest_curve_distance: Option<f32>,
    pub(crate) nearest_acquisition_radius: Option<f32>,
    pub(crate) hysteresis_retained: bool,
}

#[cfg(test)]
pub(crate) fn junction_arc_target_at_point(
    targets: &[ComposedJunctionArcTarget],
    track_id: TrackId,
    point: egui::Pos2,
    current: Option<JunctionArcHover>,
    content_revision: u64,
) -> Option<JunctionArcHover> {
    junction_arc_hit_test_at_point(targets, track_id, point, current, content_revision).hover
}

pub(crate) fn junction_arc_hit_test_at_point(
    targets: &[ComposedJunctionArcTarget],
    track_id: TrackId,
    point: egui::Pos2,
    current: Option<JunctionArcHover>,
    content_revision: u64,
) -> JunctionArcHitTestResult {
    let bounds_candidates = std::cell::Cell::new(0_usize);
    let curve_evaluations = std::cell::Cell::new(0_usize);
    let stale_revision_rejections = std::cell::Cell::new(0_usize);
    let nearest_curve_distance = std::cell::Cell::new(None::<f32>);
    let nearest_acquisition_radius = std::cell::Cell::new(None::<f32>);
    let candidate = |target: &ComposedJunctionArcTarget, exit_margin: f32| {
        if target.key.track_id != track_id {
            return None;
        }
        if target.content_revision != content_revision {
            stale_revision_rejections.set(stale_revision_rejections.get().saturating_add(1));
            return None;
        }
        let radius = (target.stroke_width * 0.5 + JUNCTION_ARC_ACQUISITION_MARGIN)
            .max(JUNCTION_ARC_MINIMUM_ACQUISITION_RADIUS)
            + exit_margin;
        let primitive = target.primitive;
        if point.x < primitive.x - radius
            || point.x > primitive.x + primitive.width + radius
            || point.y < primitive.y - radius
            || point.y > primitive.y + primitive.height + radius
        {
            return None;
        }
        bounds_candidates.set(bounds_candidates.get().saturating_add(1));
        curve_evaluations.set(curve_evaluations.get().saturating_add(1));
        let distance = junction_arc_distance_to_point(primitive, point.x, point.y)?;
        if nearest_curve_distance
            .get()
            .is_none_or(|nearest| distance < nearest)
        {
            nearest_curve_distance.set(Some(distance));
            nearest_acquisition_radius.set(Some(radius));
        }
        (distance <= radius).then_some(JunctionArcHover {
            key: target.key,
            interaction_token: target.interaction_token,
            distance_points: distance,
            content_revision,
        })
    };
    let best = targets
        .iter()
        .filter_map(|target| candidate(target, 0.0).map(|hit| (hit, target.draw_order)))
        .min_by(|(left, left_order), (right, right_order)| {
            if (left.distance_points - right.distance_points).abs()
                <= JUNCTION_ARC_DISTANCE_TIE_EPSILON
            {
                right_order
                    .cmp(left_order)
                    .then_with(|| left.key.cmp(&right.key))
            } else {
                left.distance_points.total_cmp(&right.distance_points)
            }
        })
        .map(|(hit, _)| hit);
    let retained = current
        .filter(|hover| hover.content_revision == content_revision)
        .and_then(|hover| {
            targets
                .iter()
                .find(|target| target.key == hover.key)
                .and_then(|target| candidate(target, JUNCTION_ARC_EXIT_MARGIN))
        });
    let (hover, hysteresis_retained) = match (retained, best) {
        (Some(retained), Some(best))
            if best.key != retained.key
                && best.distance_points + JUNCTION_ARC_TRANSFER_ADVANTAGE
                    < retained.distance_points =>
        {
            (Some(best), false)
        }
        (Some(retained), best) => (
            Some(retained),
            best.is_none_or(|best| best.key != retained.key),
        ),
        (None, best) => (best, false),
    };
    JunctionArcHitTestResult {
        hover,
        gate: SashimiHitTestGate::Tested,
        lane_track_id: Some(track_id),
        total_targets: targets.len(),
        track_targets: targets
            .iter()
            .filter(|target| target.key.track_id == track_id)
            .count(),
        current_revision_targets: targets
            .iter()
            .filter(|target| {
                target.key.track_id == track_id && target.content_revision == content_revision
            })
            .count(),
        bounds_candidates: bounds_candidates.get(),
        curve_evaluations: curve_evaluations.get(),
        stale_revision_rejections: stale_revision_rejections.get(),
        nearest_curve_distance: nearest_curve_distance.get(),
        nearest_acquisition_radius: nearest_acquisition_radius.get(),
        hysteresis_retained,
    }
}

pub(crate) fn alignment_record_index(object_id: u32) -> Option<usize> {
    let record_id = record_object_id(object_id);
    (record_id > 0 && record_id < 1_000_000)
        .then(|| usize::try_from(record_id - 1).ok())
        .flatten()
}

#[cfg(test)]
pub(crate) fn alignment_track_at_content_y(
    frames: &[TrackFrame],
    content_y: f32,
) -> Option<TrackId> {
    frames.iter().find_map(|frame| {
        (content_y >= frame.top && content_y < frame.bottom)
            .then_some(frame.key)
            .and_then(|key| match key {
                TrackFrameKey::Alignment(id) => Some(id),
                TrackFrameKey::Reference
                | TrackFrameKey::Group { .. }
                | TrackFrameKey::Auxiliary(_) => None,
            })
    })
}

#[cfg(test)]
pub(crate) fn alignment_lane_at_screen_y(
    frames: &[TrackFrame],
    alignment_subtrack: AlignmentSubtrack,
    screen_y: f32,
    vertical_scroll: f32,
    annotation_scroll: f32,
    scroll_clip_top: f32,
    annotation_clip_top: f32,
    viewport_top: f32,
    viewport_bottom: f32,
) -> Option<TrackId> {
    if screen_y < viewport_top || screen_y >= viewport_bottom {
        return None;
    }
    frames.iter().find_map(|frame| {
        let TrackFrameKey::Alignment(id) = frame.key else {
            return None;
        };
        let annotation_frame = frame.channel == ui::TrackPane::Annotation;
        if annotation_frame && screen_y >= scroll_clip_top {
            return None;
        }
        if !annotation_frame && screen_y < scroll_clip_top {
            return None;
        }
        frame.subtracks.iter().find_map(|subtrack| {
            if subtrack.alignment_subtrack != Some(alignment_subtrack) || subtrack.collapsed {
                return None;
            }
            let (scroll, clip_top, frame_bottom) = if annotation_frame {
                (
                    annotation_scroll,
                    annotation_clip_top,
                    frame.bottom - annotation_scroll,
                )
            } else {
                (
                    vertical_scroll,
                    scroll_clip_top,
                    frame.bottom - vertical_scroll,
                )
            };
            if screen_y < clip_top || screen_y >= frame_bottom {
                return None;
            }
            let y = screen_y + scroll;
            (y >= subtrack.top && y < subtrack.bottom).then_some(id)
        })
    })
}

/// Resolves an alignment sublane from the canonical pane layout used by the
/// gutter and canvas. This avoids reconstructing pane identity and clipping
/// from the renderer's raw frame cache.
pub(crate) fn alignment_lane_in_pane_layout(
    layout: &ui::PaneLayoutSnapshot,
    label: &str,
    screen_y: f32,
) -> Option<TrackId> {
    if screen_y < layout.pane_rect.top() || screen_y >= layout.pane_rect.bottom() {
        return None;
    }
    layout.items.iter().find_map(|item| {
        let TrackFrameKey::Alignment(track_id) = item.frame.key else {
            return None;
        };
        if !item.effective_content_visible || !item.clip_rect.contains(egui::pos2(0.0, screen_y)) {
            return None;
        }
        item.frame.subtracks.iter().find_map(|subtrack| {
            if subtrack.label != label || subtrack.collapsed {
                return None;
            }
            let top = subtrack.top - layout.scroll_points;
            let bottom = subtrack.bottom - layout.scroll_points;
            (screen_y >= top
                && screen_y < bottom
                && screen_y >= item.clip_rect.top()
                && screen_y < item.clip_rect.bottom())
            .then_some(track_id)
        })
    })
}

pub(crate) fn alignment_record_at_point(display: &DisplayList, x: f32, y: f32) -> Option<usize> {
    display
        .rectangles
        .iter()
        .rev()
        .find(|rectangle| {
            alignment_record_index(rectangle.object_id).is_some()
                && x >= rectangle.x
                && x <= rectangle.x + rectangle.width
                && y >= rectangle.y
                && y <= rectangle.y + rectangle.height
        })
        .and_then(|rectangle| alignment_record_index(rectangle.object_id))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReadEventHit {
    pub(crate) operation_index: usize,
    pub(crate) operation_offset: Option<u32>,
}

pub(crate) fn find_read_event(
    record: &AlignmentRecord,
    reference: Option<&ReferenceSlice>,
    position: f64,
    tolerance: f64,
    include_substitutions: bool,
) -> Option<ReadEventHit> {
    let mut reference_position = record.start;
    let mut read_position = 0_usize;
    let mut best = None::<(f64, usize)>;
    let genomic_position =
        (position.is_finite() && position >= 0.0).then(|| position.floor() as u64);
    for (index, operation) in record.cigar.iter().enumerate() {
        let length = u64::from(operation.len);
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                let end = reference_position.saturating_add(length);
                if include_substitutions
                    && let Some(genomic_position) = genomic_position
                    && (reference_position..end).contains(&genomic_position)
                {
                    let offset = u32::try_from(genomic_position - reference_position).ok()?;
                    let aligned_read_position = read_position.saturating_add(offset as usize);
                    let reference_base =
                        reference.and_then(|slice| slice.base_at(genomic_position));
                    let observed = match &record.sequence {
                        SequenceData::Present { bases, .. } => {
                            bases.get(aligned_read_position).copied()
                        }
                        SequenceData::NotLoaded | SequenceData::Missing => None,
                    };
                    let classification =
                        classify_aligned_base(operation.kind, observed, reference_base);
                    if matches!(
                        classification,
                        AlignedBaseClassification::Substitution
                            | AlignedBaseClassification::DeclaredSubstitution
                            | AlignedBaseClassification::EncodingConflict
                    ) {
                        return Some(ReadEventHit {
                            operation_index: index,
                            operation_offset: Some(offset),
                        });
                    }
                }
                reference_position = reference_position.saturating_add(length);
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(length);
                if position >= reference_position as f64 && position < end as f64 {
                    return Some(ReadEventHit {
                        operation_index: index,
                        operation_offset: None,
                    });
                }
                reference_position = end;
            }
            CigarOpKind::Insertion | CigarOpKind::SoftClip => {
                let distance = (position - reference_position as f64).abs();
                if distance <= tolerance
                    && best.is_none_or(|(best_distance, _)| distance < best_distance)
                {
                    best = Some((distance, index));
                }
                read_position = read_position.saturating_add(operation.len as usize);
            }
            CigarOpKind::HardClip | CigarOpKind::Padding => {
                let distance = (position - reference_position as f64).abs();
                if distance <= tolerance
                    && best.is_none_or(|(best_distance, _)| distance < best_distance)
                {
                    best = Some((distance, index));
                }
            }
        }
    }
    best.map(|(_, operation_index)| ReadEventHit {
        operation_index,
        operation_offset: None,
    })
}

pub(crate) fn pileup_position_from_pointer(
    query: &AlignmentQuery,
    viewport: Viewport,
    genomic_canvas: egui::Rect,
    pointer: egui::Pos2,
) -> Option<u64> {
    if !genomic_canvas.contains(pointer) {
        return None;
    }
    let local_x = f64::from(pointer.x - genomic_canvas.left());
    let position = viewport.genomic_at_pixel(local_x).floor() as u64;
    query
        .pileup
        .bin_at(query.interval, position)
        .map(|_| position)
}

pub(crate) fn modification_position_from_pointer(
    query: &AlignmentQuery,
    viewport: Viewport,
    genomic_canvas: egui::Rect,
    pointer: egui::Pos2,
) -> Option<u64> {
    if !genomic_canvas.contains(pointer) {
        return None;
    }
    let local_x = f64::from(pointer.x - genomic_canvas.left());
    let position = viewport.genomic_at_pixel(local_x).floor() as u64;
    query
        .pileup
        .modifications
        .bin_index_at(query.interval, position)
        .map(|_| position)
}
