#[cfg(test)]
use crate::ui::tracks::geometry::clipped_frame_screen_bounds;

#[cfg(test)]
use crate::ui::{TrackFrame, TrackFrameKey};
#[cfg(test)]
use rgv_core::TrackId;

#[cfg(test)]
pub fn track_drop_index(
    frames: &[TrackFrame],
    track_order: &[TrackId],
    pointer_y: f32,
    vertical_scroll: f32,
    scroll_clip_top: f32,
) -> usize {
    let target = frames
        .iter()
        .filter_map(|frame| {
            if matches!(frame.key, TrackFrameKey::Group { .. }) {
                return None;
            }
            let bounds = clipped_frame_screen_bounds(
                frame,
                vertical_scroll,
                scroll_clip_top,
                f32::NEG_INFINITY,
                f32::INFINITY,
            )?;
            (pointer_y < (bounds.top + bounds.bottom) * 0.5).then_some(frame.key)
        })
        .next();
    match target {
        Some(TrackFrameKey::Group { .. }) => unreachable!("group headers are not reorder targets"),
        Some(TrackFrameKey::Reference) => 0,
        Some(TrackFrameKey::Auxiliary(target)) => track_order
            .iter()
            .position(|candidate| *candidate == target)
            .unwrap_or_else(|| track_order.len().saturating_sub(1)),
        Some(TrackFrameKey::Alignment(target)) => track_order
            .iter()
            .position(|candidate| *candidate == target)
            .unwrap_or(0),
        None => track_order.len().saturating_sub(1),
    }
}

#[cfg(test)]
pub fn keyboard_reorder_index(current: usize, count: usize, direction: i32) -> usize {
    match direction.cmp(&0) {
        std::cmp::Ordering::Less => current.saturating_sub(1),
        std::cmp::Ordering::Greater => current.saturating_add(1).min(count.saturating_sub(1)),
        std::cmp::Ordering::Equal => current.min(count.saturating_sub(1)),
    }
}

#[cfg(test)]
pub fn resized_track_height(origin: f32, drag_delta: f32) -> f32 {
    (origin + drag_delta).clamp(40.0, 600.0)
}

pub fn resized_annotation_track_height(origin: f32, drag_delta: f32) -> f32 {
    (origin + drag_delta).clamp(80.0, 600.0)
}

pub fn resized_read_track_height(origin: f32, drag_delta: f32) -> f32 {
    (origin + drag_delta).clamp(40.0, 1_200.0)
}

pub fn resized_read_pane_height(
    current_height: f32,
    divider_delta: f32,
    viewport_height: f32,
) -> f32 {
    let maximum = (viewport_height - 80.0).max(100.0);
    (current_height - divider_delta).clamp(100.0, maximum)
}
