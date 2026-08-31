use super::*;

pub(crate) const JUNCTION_ARC_MINIMUM_ACQUISITION_RADIUS: f32 = 4.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct JunctionArcHitTestResult {
    pub(crate) hover: Option<JunctionArcHover>,
    pub(crate) native_authority: bool,
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
