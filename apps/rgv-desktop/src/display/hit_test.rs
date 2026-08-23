use super::*;

pub(crate) fn alignment_record_index(object_id: u32) -> Option<usize> {
    let record_id = record_object_id(object_id);
    (record_id > 0 && record_id < 1_000_000)
        .then(|| usize::try_from(record_id - 1).ok())
        .flatten()
}

pub(crate) fn alignment_track_at_content_y(
    frames: &[TrackFrame],
    content_y: f32,
) -> Option<TrackId> {
    frames.iter().find_map(|frame| {
        (content_y >= frame.top && content_y < frame.bottom)
            .then_some(frame.key)
            .and_then(|key| match key {
                TrackFrameKey::Alignment(id) => Some(id),
                TrackFrameKey::Reference | TrackFrameKey::Auxiliary(_) => None,
            })
    })
}

pub(crate) fn alignment_lane_at_screen_y(
    frames: &[TrackFrame],
    label: &str,
    screen_y: f32,
    vertical_scroll: f32,
) -> Option<TrackId> {
    frames.iter().find_map(|frame| {
        let TrackFrameKey::Alignment(id) = frame.key else {
            return None;
        };
        frame.subtracks.iter().find_map(|subtrack| {
            if subtrack.label != label {
                return None;
            }
            let y = if frame
                .frozen_bottom
                .is_some_and(|frozen_bottom| subtrack.bottom <= frozen_bottom)
            {
                screen_y
            } else {
                screen_y + vertical_scroll
            };
            (y >= subtrack.top && y < subtrack.bottom).then_some(id)
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

pub(crate) fn find_read_event(
    record: &AlignmentRecord,
    position: f64,
    tolerance: f64,
) -> Option<usize> {
    let mut reference_position = record.start;
    let mut best = None::<(f64, usize)>;
    for (index, operation) in record.cigar.iter().enumerate() {
        let length = u64::from(operation.len);
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                reference_position = reference_position.saturating_add(length);
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(length);
                if position >= reference_position as f64 && position < end as f64 {
                    return Some(index);
                }
                reference_position = end;
            }
            CigarOpKind::Insertion
            | CigarOpKind::SoftClip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {
                let distance = (position - reference_position as f64).abs();
                if distance <= tolerance
                    && best.is_none_or(|(best_distance, _)| distance < best_distance)
                {
                    best = Some((distance, index));
                }
            }
        }
    }
    best.map(|(_, index)| index)
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

pub(crate) fn junction_at_position(
    summary: &rgv_hts::JunctionSummary,
    minimum_depth: u32,
    position: u64,
) -> Option<(u64, u64)> {
    summary
        .junctions
        .iter()
        .filter(|junction| {
            junction.depth >= minimum_depth && position >= junction.start && position < junction.end
        })
        .max_by_key(|junction| {
            (
                junction.depth,
                u64::MAX.saturating_sub(junction.end.saturating_sub(junction.start)),
            )
        })
        .map(|junction| (junction.start, junction.end))
}
