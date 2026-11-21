use crate::ui::{TrackFrame, TrackSubtrack};
use egui::{Align2, Color32, FontId, Rect, Ui, pos2};
use rgv_layout::TextPrimitive;

pub(super) fn boundary_inside_clip(boundary: f32, clip_top: f32, clip_bottom: f32) -> bool {
    boundary >= clip_top && boundary <= clip_bottom
}

pub(super) fn frame_screen_bounds(frame: &TrackFrame, vertical_scroll: f32) -> (f32, f32) {
    (frame.top - vertical_scroll, frame.bottom - vertical_scroll)
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg(test)]
pub(super) struct VerticalScreenBounds {
    pub(super) top: f32,
    pub(super) bottom: f32,
}

#[cfg(test)]
pub(super) fn clipped_frame_screen_bounds(
    frame: &TrackFrame,
    vertical_scroll: f32,
    scroll_clip_top: f32,
    viewport_top: f32,
    viewport_bottom: f32,
) -> Option<VerticalScreenBounds> {
    let (top, bottom) = frame_screen_bounds(frame, vertical_scroll);
    let clip_top = scroll_clip_top;
    let bounds = VerticalScreenBounds {
        top: top.max(clip_top).max(viewport_top),
        bottom: bottom.min(viewport_bottom),
    };
    (bounds.bottom > bounds.top).then_some(bounds)
}

/// Converts a visible screen-space row back into its frame-local content
/// coordinate using the same pane scroll as rendering.
pub fn frame_content_y_at_screen_y(
    frame: &TrackFrame,
    screen_y: f32,
    vertical_scroll: f32,
    scroll_clip_top: f32,
    viewport_top: f32,
    viewport_bottom: f32,
) -> Option<f32> {
    if screen_y < viewport_top || screen_y >= viewport_bottom {
        return None;
    }
    if screen_y < scroll_clip_top {
        return None;
    }
    let content_y = screen_y + vertical_scroll;
    (content_y >= frame.top && content_y < frame.bottom).then_some(content_y)
}

pub(super) fn reference_orientation_accessibility(
    label: &str,
) -> Option<(&'static str, &'static str)> {
    if label.starts_with('>') {
        Some((
            "Reference sequence, forward genomic direction",
            "Reference sequence is displayed in the forward genomic direction",
        ))
    } else if label.starts_with('<') {
        Some((
            "Reference sequence, reverse-complement direction",
            "Reference sequence is displayed as the reverse complement",
        ))
    } else {
        None
    }
}

pub(super) fn subtrack_screen_bounds(
    _frame: &TrackFrame,
    subtrack: &TrackSubtrack,
    vertical_scroll: f32,
) -> (f32, f32) {
    (
        subtrack.top - vertical_scroll,
        subtrack.bottom - vertical_scroll,
    )
}

pub fn paint_genomic_text(ui: &Ui, clip_rect: Rect, texts: &[TextPrimitive]) {
    let painter = ui.painter().with_clip_rect(clip_rect);
    for text in texts {
        let color = crate::theme::adapt(Color32::from_rgba_unmultiplied(
            (text.color.red.clamp(0.0, 1.0) * 255.0).round() as u8,
            (text.color.green.clamp(0.0, 1.0) * 255.0).round() as u8,
            (text.color.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
            (text.color.alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        ));
        painter.text(
            pos2(text.x, text.y),
            Align2::CENTER_CENTER,
            &text.text,
            FontId::monospace(text.font_size),
            color,
        );
    }
}
