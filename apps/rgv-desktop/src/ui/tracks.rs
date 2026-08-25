use std::sync::Arc;

use egui::{
    Align2, Color32, FontId, Galley, Rect, Sense, Stroke, Ui, pos2,
    text::{LayoutJob, TextFormat, TextWrapping},
};
use rgv_core::{TrackId, TrackKind};
use rgv_layout::{DisplayList, TextPrimitive};

use super::CanvasGeometry;
use crate::{MAX_VERTICAL_DENSITY, MIN_VERTICAL_DENSITY};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrackFrameKey {
    Reference,
    Alignment(TrackId),
    Auxiliary(TrackId),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AlignmentSubtrack {
    Reference,
    Coverage,
    Sashimi,
    Modifications,
    Reads,
}

impl AlignmentSubtrack {
    pub const ALL: [Self; 5] = [
        Self::Reference,
        Self::Coverage,
        Self::Sashimi,
        Self::Modifications,
        Self::Reads,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Reference => "Reference",
            Self::Coverage => "Coverage",
            Self::Sashimi => "Sashimi",
            Self::Modifications => "Modifications",
            Self::Reads => "Reads",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|subtrack| subtrack.label() == label)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackSubtrack {
    pub label: String,
    pub detail: Option<String>,
    pub top: f32,
    pub bottom: f32,
    /// A compact placeholder for a hidden alignment lane. The gutter keeps
    /// its label and closed-eye control so visibility can be restored in place.
    pub collapsed: bool,
    /// Stable owner for a composite member row. Ordinary informational and
    /// alignment subtrack rows derive their controls from the parent frame.
    pub composite_member: Option<TrackId>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackFrame {
    pub key: TrackFrameKey,
    pub name: String,
    pub kind: TrackKind,
    pub source_label: Option<String>,
    pub top: f32,
    pub bottom: f32,
    pub visible: bool,
    pub collapsed: bool,
    /// Bottom of a frozen header region. Content below this point scrolls,
    /// while the frame header and preceding subtracks remain pinned.
    pub frozen_bottom: Option<f32>,
    pub subtracks: Vec<TrackSubtrack>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackUiAction {
    Select(TrackFrameKey),
    OpenSettings(TrackFrameKey),
    ToggleVisibility(TrackFrameKey),
    ToggleCollapsed(TrackFrameKey),
    ToggleAlignmentSubtrack(TrackId, AlignmentSubtrack),
    ToggleCompositeMember(TrackId),
    Export(TrackFrameKey),
    Remove(TrackFrameKey),
    ReorderDropped(TrackId, f32),
    ReorderKeyboard(TrackId, i32),
    ResizeStarted(TrackId),
    ResizeDragged(TrackId, f32),
    ResizeStopped(TrackId),
    ResizeKeyboard(TrackId, f32),
    ResizeAnnotationStarted(TrackFrameKey, f32),
    ResizeAnnotationDragged(TrackFrameKey, f32),
    ResizeAnnotationStopped(TrackFrameKey),
    ResizeAnnotationKeyboard(TrackFrameKey, f32, f32),
    ResizeReadTrackStarted(TrackId, f32),
    ResizeReadTrackDragged(TrackId, f32),
    ResizeReadTrackStopped(TrackId),
    ResizeReadTrackKeyboard(TrackId, f32, f32),
    ResizeReadPaneStarted,
    ResizeReadPaneDragged(f32),
    ResizeReadPaneStopped,
    ResizeReadPaneKeyboard(f32),
}

#[derive(Default)]
pub struct TrackGutterResult {
    pub actions: Vec<TrackUiAction>,
}

const GUTTER_TEXT_GAP: f32 = 6.0;
const MAX_GUTTER_LABEL_FRACTION: f32 = 0.55;
const ANNOTATION_PANE_GUTTER_COLOR: Color32 = Color32::from_rgb(234, 241, 248);
const READ_PANE_GUTTER_COLOR: Color32 = Color32::from_gray(249);

fn gutter_row_widths(label_width: f32, detail_width: f32, available_width: f32) -> (f32, f32) {
    let available_width = available_width.max(0.0);
    if detail_width <= f32::EPSILON {
        return (label_width.min(available_width), 0.0);
    }
    if label_width + GUTTER_TEXT_GAP + detail_width <= available_width {
        return (label_width, detail_width);
    }
    let text_width = (available_width - GUTTER_TEXT_GAP).max(0.0);
    let label_width = label_width.min(text_width * MAX_GUTTER_LABEL_FRACTION);
    (label_width, (text_width - label_width).max(0.0))
}

fn layout_elided_text(
    painter: &egui::Painter,
    text: &str,
    font_id: FontId,
    color: Color32,
    maximum_width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob {
        wrap: TextWrapping::truncate_at_width(maximum_width.max(0.0)),
        ..LayoutJob::default()
    };
    job.append(text, 0.0, TextFormat::simple(font_id, color));
    painter.layout_job(job)
}

fn paint_elided_text(
    painter: &egui::Painter,
    text: &str,
    font_id: FontId,
    color: Color32,
    maximum_width: f32,
    anchor: egui::Pos2,
    right_aligned: bool,
) {
    if maximum_width <= f32::EPSILON {
        return;
    }
    let galley = layout_elided_text(painter, text, font_id, color, maximum_width);
    let left = if right_aligned {
        anchor.x - galley.size().x
    } else {
        anchor.x
    };
    painter.galley(pos2(left, anchor.y - galley.size().y * 0.5), galley, color);
}

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

pub fn keyboard_reorder_index(current: usize, count: usize, direction: i32) -> usize {
    match direction.cmp(&0) {
        std::cmp::Ordering::Less => current.saturating_sub(1),
        std::cmp::Ordering::Greater => current.saturating_add(1).min(count.saturating_sub(1)),
        std::cmp::Ordering::Equal => current.min(count.saturating_sub(1)),
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarGeometry {
    pub track: Rect,
    pub thumb: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarOverviewBin {
    pub density: f32,
    pub kind: Option<TrackKind>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScrollbarOverview {
    pub bins: Vec<ScrollbarOverviewBin>,
    /// Track boundaries as fractions of the complete vertical content span.
    pub boundaries: Vec<f32>,
}

/// Builds a bounded semantic miniature of the vertical content stack. The
/// model keeps track identity out of the renderer and records only relative
/// occupancy, so it is safe to expose through ordinary UI diagnostics.
pub fn scrollbar_overview(
    frames: &[TrackFrame],
    display: &DisplayList,
    content_top: f32,
    content_bottom: f32,
    scrollable_top: f32,
    scrollable_offset: f32,
    genomic_width: f32,
    maximum_bins: usize,
) -> ScrollbarOverview {
    let span = (content_bottom - content_top).max(0.0);
    if span <= f32::EPSILON || maximum_bins == 0 {
        return ScrollbarOverview::default();
    }
    let bin_count = maximum_bins.min((span / 4.0).ceil().max(1.0) as usize);
    let mut activity = vec![0.0_f32; bin_count];
    let mut bins = vec![
        ScrollbarOverviewBin {
            density: 0.0,
            kind: None,
        };
        bin_count
    ];
    let bin_height = span / bin_count as f32;

    for (index, bin) in bins.iter_mut().enumerate() {
        let y = content_top + (index as f32 + 0.5) * bin_height;
        bin.kind = frames
            .iter()
            .find(|frame| {
                if !frame.visible {
                    return false;
                }
                let top = if frame.top >= scrollable_top {
                    frame.top + scrollable_offset
                } else {
                    frame.top
                };
                let bottom = if frame.bottom > scrollable_top {
                    frame.bottom + scrollable_offset
                } else {
                    frame.bottom
                };
                y >= top && y < bottom
            })
            .map(|frame| frame.kind);
    }
    let width = genomic_width.max(1.0);
    for rectangle in &display.rectangles {
        let top = (rectangle.y + scrollable_offset).max(content_top);
        let bottom = (rectangle.y + scrollable_offset + rectangle.height).min(content_bottom);
        if bottom <= top || rectangle.color.alpha <= f32::EPSILON {
            continue;
        }
        let first = (((top - content_top) / span) * bin_count as f32)
            .floor()
            .clamp(0.0, (bin_count - 1) as f32) as usize;
        let last = ((((bottom - content_top) / span) * bin_count as f32).ceil() as usize)
            .saturating_sub(1)
            .min(bin_count - 1);
        let horizontal_occupancy = (rectangle.width / width).clamp(0.015, 1.0);
        for value in &mut activity[first..=last] {
            *value += horizontal_occupancy;
        }
    }
    let maximum = activity.iter().copied().fold(0.0_f32, f32::max);
    if maximum > f32::EPSILON {
        let denominator = maximum.ln_1p();
        for (bin, value) in bins.iter_mut().zip(activity) {
            bin.density = (value.ln_1p() / denominator).clamp(0.0, 1.0);
        }
    }
    let mut boundaries = frames
        .iter()
        .filter(|frame| frame.visible)
        .map(|frame| {
            let top = if frame.top >= scrollable_top {
                frame.top + scrollable_offset
            } else {
                frame.top
            };
            ((top - content_top) / span).clamp(0.0, 1.0)
        })
        .collect::<Vec<_>>();
    boundaries.sort_by(f32::total_cmp);
    boundaries.dedup_by(|left, right| (*left - *right).abs() < 0.002);
    ScrollbarOverview { bins, boundaries }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ScrollbarInteraction {
    pub pointer_down: bool,
    pub drag_started: bool,
    pub dragging: bool,
    pub drag_stopped: bool,
    pub drag_delta: egui::Vec2,
    pub pointer: Option<egui::Pos2>,
    pub keyboard_density_pixels: f32,
}

pub fn scrollbar_geometry(rect: Rect, maximum: f32, offset: f32) -> ScrollbarGeometry {
    let fraction = if maximum <= f32::EPSILON {
        0.0
    } else {
        offset.clamp(0.0, maximum) / maximum
    };
    scrollbar_geometry_with_visible_fraction(rect, maximum, rect.height(), 1.0, fraction)
}

/// Paints the ordinary annotation-stack scrollbar. Unlike the adjacent read
/// navigator it has one axis and never changes density.
pub fn paint_annotation_scrollbar(
    ui: &mut Ui,
    rect: Rect,
    maximum: f32,
    offset: &mut f32,
    overview: Option<&ScrollbarOverview>,
) -> ScrollbarInteraction {
    let maximum = maximum.max(0.0);
    *offset = offset.clamp(0.0, maximum);
    let response = ui
        .interact(
            rect,
            egui::Id::new("annotation-vertical-scrollbar"),
            Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::ResizeVertical)
        .on_hover_text("Annotation tracks · drag up/down to scroll");
    let scroll_percent = if maximum <= f32::EPSILON {
        0.0
    } else {
        f64::from(*offset / maximum * 100.0)
    };
    response.widget_info(|| {
        egui::WidgetInfo::slider(
            true,
            scroll_percent,
            "Annotation track scrollbar; arrow up and down scroll, Page Up and Page Down scroll by a page, Home and End reach the stack boundaries",
        )
    });
    if response.has_focus() {
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                *offset -= 40.0;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                *offset += 40.0;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                *offset -= rect.height() * 0.8;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                *offset += rect.height() * 0.8;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                *offset = 0.0;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                *offset = maximum;
            }
        });
        *offset = offset.clamp(0.0, maximum);
    }
    let mut geometry = scrollbar_geometry(rect, maximum, *offset);
    let drag_state_id = egui::Id::new("annotation-scrollbar-drag-state");
    if response.drag_started()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let grab = if geometry.thumb.contains(pointer) {
            pointer.y - geometry.thumb.top()
        } else {
            geometry.thumb.height() * 0.5
        };
        ui.ctx()
            .data_mut(|data| data.insert_temp(drag_state_id, (*offset, grab)));
    }
    if response.dragged()
        && let Some(pointer) = response.interact_pointer_pos()
        && let Some((origin, grab)) = ui
            .ctx()
            .data(|data| data.get_temp::<(f32, f32)>(drag_state_id))
    {
        let origin_geometry = scrollbar_geometry(rect, maximum, origin);
        *offset = scrollbar_offset_at_grab(origin_geometry, maximum, pointer.y, grab);
        geometry = scrollbar_geometry(rect, maximum, *offset);
    }
    if response.drag_stopped() {
        ui.ctx().data_mut(|data| {
            data.remove::<(f32, f32)>(drag_state_id);
        });
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, Color32::from_gray(250));
    painter.rect_filled(geometry.track, 0.0, Color32::WHITE);
    if let Some(overview) = overview.filter(|overview| !overview.bins.is_empty()) {
        let bin_height = geometry.track.height() / overview.bins.len() as f32;
        for (index, bin) in overview.bins.iter().enumerate() {
            let top = geometry.track.top() + index as f32 * bin_height;
            let bottom = if index + 1 == overview.bins.len() {
                geometry.track.bottom()
            } else {
                top + bin_height + 0.5
            };
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(geometry.track.left(), top),
                    pos2(geometry.track.right(), bottom),
                ),
                0.0,
                scrollbar_overview_color(bin.kind, bin.density),
            );
        }
        for boundary in &overview.boundaries {
            let y = egui::lerp(geometry.track.y_range(), boundary.clamp(0.0, 1.0));
            painter.line_segment(
                [
                    pos2(geometry.track.left(), y),
                    pos2(geometry.track.right(), y),
                ],
                Stroke::new(0.75, Color32::from_gray(72)),
            );
        }
    }
    painter.rect_stroke(
        geometry.track,
        0.0,
        Stroke::new(0.75, Color32::from_gray(95)),
        egui::StrokeKind::Inside,
    );
    painter.rect_filled(
        geometry.thumb,
        1.0,
        Color32::from_rgba_unmultiplied(75, 91, 112, 42),
    );
    painter.rect_stroke(
        geometry.thumb,
        1.0,
        Stroke::new(1.5, Color32::from_rgb(75, 91, 112)),
        egui::StrokeKind::Outside,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.shrink(1.0),
            1.0,
            Stroke::new(2.0, Color32::BLACK),
            egui::StrokeKind::Inside,
        );
    }
    ScrollbarInteraction {
        pointer_down: response.is_pointer_button_down_on() || response.dragged(),
        drag_started: response.drag_started(),
        dragging: response.dragged(),
        drag_stopped: response.drag_stopped(),
        drag_delta: response.drag_delta(),
        pointer: response.interact_pointer_pos(),
        keyboard_density_pixels: 0.0,
    }
}

#[cfg(test)]
pub fn scrollbar_geometry_for_density(
    rect: Rect,
    maximum: f32,
    offset: f32,
    density: f32,
) -> ScrollbarGeometry {
    scrollbar_geometry_for_density_in_viewport(rect, maximum, offset, density, rect.height())
}

pub fn scrollbar_geometry_for_density_in_viewport(
    rect: Rect,
    maximum: f32,
    offset: f32,
    density: f32,
    camera_viewport_height: f32,
) -> ScrollbarGeometry {
    let fraction = if maximum <= f32::EPSILON {
        0.0
    } else {
        offset.clamp(0.0, maximum) / maximum
    };
    if maximum <= f32::EPSILON {
        scrollbar_geometry_with_visible_fraction(
            rect,
            maximum,
            camera_viewport_height,
            1.0,
            fraction,
        )
    } else {
        scrollbar_geometry_for_density_at_fraction_in_viewport(
            rect,
            maximum,
            density,
            fraction,
            camera_viewport_height,
        )
    }
}

#[cfg(test)]
pub fn scrollbar_geometry_for_density_at_fraction(
    rect: Rect,
    maximum: f32,
    density: f32,
    scroll_fraction: f32,
) -> ScrollbarGeometry {
    scrollbar_geometry_for_density_at_fraction_in_viewport(
        rect,
        maximum,
        density,
        scroll_fraction,
        rect.height(),
    )
}

pub fn scrollbar_geometry_for_density_at_fraction_in_viewport(
    rect: Rect,
    maximum: f32,
    density: f32,
    scroll_fraction: f32,
    camera_viewport_height: f32,
) -> ScrollbarGeometry {
    // The thumb represents the actual viewport, not a prospective camera position.
    // When the content fits there is no vertical range outside that viewport, so a
    // saved or in-progress density anchor must not contract or position the thumb.
    if maximum <= f32::EPSILON {
        return scrollbar_geometry_with_visible_fraction(
            rect,
            maximum,
            camera_viewport_height,
            1.0,
            0.0,
        );
    }
    let density = density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
    scrollbar_geometry_with_visible_fraction(
        rect,
        maximum,
        camera_viewport_height,
        (MIN_VERTICAL_DENSITY / density).clamp(0.0, 1.0),
        scroll_fraction,
    )
}

fn scrollbar_geometry_with_visible_fraction(
    rect: Rect,
    maximum: f32,
    camera_viewport_height: f32,
    scale_visible_fraction: f32,
    scroll_fraction: f32,
) -> ScrollbarGeometry {
    let track = rect.shrink2(egui::vec2(6.0, 5.0));
    let maximum = maximum.max(0.0);
    let viewport_height = camera_viewport_height.max(1.0);
    let content_height = viewport_height + maximum;
    let visible_fraction = (viewport_height / content_height).min(scale_visible_fraction);
    let thumb_height =
        (track.height() * visible_fraction).clamp(32.0_f32.min(track.height()), track.height());
    let travel = (track.height() - thumb_height).max(0.0);
    let fraction = scroll_fraction.clamp(0.0, 1.0);
    let thumb_top = track.top() + travel * fraction;
    let thumb = Rect::from_min_size(
        pos2(track.left(), thumb_top),
        egui::vec2(track.width(), thumb_height),
    );
    ScrollbarGeometry { track, thumb }
}

pub fn scrollbar_fraction_at_grab(
    geometry: ScrollbarGeometry,
    pointer_y: f32,
    grab_fraction: f32,
) -> f32 {
    let travel = (geometry.track.height() - geometry.thumb.height()).max(0.0);
    if travel <= f32::EPSILON {
        return grab_fraction.clamp(0.0, 1.0);
    }
    let requested_top = pointer_y - grab_fraction.clamp(0.0, 1.0) * geometry.thumb.height();
    ((requested_top - geometry.track.top()) / travel).clamp(0.0, 1.0)
}

#[cfg(test)]
pub fn scrollbar_offset_at_pointer(
    geometry: ScrollbarGeometry,
    maximum: f32,
    pointer_y: f32,
) -> f32 {
    scrollbar_offset_at_grab(geometry, maximum, pointer_y, geometry.thumb.height() * 0.5)
}

pub fn scrollbar_offset_at_grab(
    geometry: ScrollbarGeometry,
    maximum: f32,
    pointer_y: f32,
    grab_offset: f32,
) -> f32 {
    let maximum = maximum.max(0.0);
    let travel = (geometry.track.height() - geometry.thumb.height()).max(0.0);
    if maximum <= f32::EPSILON || travel <= f32::EPSILON {
        return 0.0;
    }
    let requested_top = pointer_y - grab_offset.clamp(0.0, geometry.thumb.height());
    let fraction = ((requested_top - geometry.track.top()) / travel).clamp(0.0, 1.0);
    fraction * maximum
}

#[allow(clippy::too_many_lines)]
pub fn paint_vertical_scrollbar(
    ui: &mut Ui,
    rect: Rect,
    maximum: f32,
    offset: &mut f32,
    density: f32,
    camera_viewport_height: f32,
    preview_scroll_fraction: Option<f32>,
    overview: Option<&ScrollbarOverview>,
) -> ScrollbarInteraction {
    let maximum = maximum.max(0.0);
    *offset = offset.clamp(0.0, maximum);
    let id = egui::Id::new("track-vertical-scrollbar");
    let response = ui
        .interact(rect, id, Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::AllScroll)
        .on_hover_text(
            "Read stack · drag up/down to scroll · drag left/right to change read-row density",
        );
    let scroll_percent = if maximum <= f32::EPSILON {
        0.0
    } else {
        f64::from(*offset / maximum * 100.0)
    };
    response.widget_info(|| {
        egui::WidgetInfo::slider(
            true,
            scroll_percent,
            "Read stack overview; arrow up and down scroll, Page Up and Page Down scroll by a page, Home and End reach the read boundaries, arrow left and right change read-row density",
        )
    });
    let mut keyboard_density_pixels = 0.0;
    if response.has_focus() {
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                *offset -= 40.0;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                *offset += 40.0;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                *offset -= rect.height() * 0.8;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                *offset += rect.height() * 0.8;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Home) {
                *offset = 0.0;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::End) {
                *offset = maximum;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                keyboard_density_pixels = -12.0;
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                keyboard_density_pixels = 12.0;
            }
        });
        *offset = offset.clamp(0.0, maximum);
    }
    let pointer_down = response.is_pointer_button_down_on() || response.dragged();
    let geometry = preview_scroll_fraction.map_or_else(
        || {
            scrollbar_geometry_for_density_in_viewport(
                rect,
                maximum,
                *offset,
                density,
                camera_viewport_height,
            )
        },
        |fraction| {
            scrollbar_geometry_for_density_at_fraction_in_viewport(
                rect,
                maximum,
                density,
                fraction,
                camera_viewport_height,
            )
        },
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, Color32::from_gray(250));
    painter.rect_filled(geometry.track, 0.0, Color32::WHITE);
    if let Some(overview) = overview.filter(|overview| !overview.bins.is_empty()) {
        let bin_height = geometry.track.height() / overview.bins.len() as f32;
        for (index, bin) in overview.bins.iter().enumerate() {
            let top = geometry.track.top() + index as f32 * bin_height;
            let bottom = if index + 1 == overview.bins.len() {
                geometry.track.bottom()
            } else {
                top + bin_height + 0.5
            };
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(geometry.track.left(), top),
                    pos2(geometry.track.right(), bottom),
                ),
                0.0,
                scrollbar_overview_color(bin.kind, bin.density),
            );
        }
        for boundary in &overview.boundaries {
            let y = egui::lerp(geometry.track.y_range(), boundary.clamp(0.0, 1.0));
            painter.line_segment(
                [
                    pos2(geometry.track.left(), y),
                    pos2(geometry.track.right(), y),
                ],
                Stroke::new(0.75, Color32::from_gray(72)),
            );
        }
    }
    painter.rect_stroke(
        geometry.track,
        0.0,
        Stroke::new(0.75, Color32::from_gray(95)),
        egui::StrokeKind::Inside,
    );
    painter.rect_filled(
        geometry.thumb,
        1.0,
        Color32::from_rgba_unmultiplied(
            45,
            92,
            175,
            if response.hovered() || response.dragged() {
                52
            } else {
                30
            },
        ),
    );
    painter.rect_stroke(
        geometry.thumb,
        1.0,
        Stroke::new(2.0, Color32::from_rgb(45, 92, 175)),
        egui::StrokeKind::Outside,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.shrink(1.0),
            1.0,
            Stroke::new(2.0, Color32::BLACK),
            egui::StrokeKind::Inside,
        );
    }
    ScrollbarInteraction {
        // `dragged` deliberately has a movement/hold threshold when a
        // response also senses clicks. Report the press separately so exact
        // thumb mapping and interaction tracing start on the first frame.
        pointer_down,
        drag_started: response.drag_started(),
        dragging: response.dragged(),
        drag_stopped: response.drag_stopped(),
        drag_delta: response.drag_delta(),
        pointer: response.interact_pointer_pos(),
        keyboard_density_pixels,
    }
}

fn scrollbar_overview_color(kind: Option<TrackKind>, density: f32) -> Color32 {
    let [red, green, blue] = match kind {
        Some(TrackKind::Alignment) => [45_u8, 92, 175],
        Some(TrackKind::Reference | TrackKind::GeneModel | TrackKind::Feature) => [60, 126, 92],
        Some(TrackKind::Variant | TrackKind::Mutation | TrackKind::Gwas) => [174, 76, 72],
        Some(TrackKind::Quantitative | TrackKind::CopyNumber) => [112, 82, 160],
        Some(TrackKind::Interaction) => [176, 112, 46],
        None => [110, 116, 124],
    };
    let strength = 0.16 + density.clamp(0.0, 1.0) * 0.72;
    let blend = |channel: u8| {
        (255.0 + (f32::from(channel) - 255.0) * strength)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgb(blend(red), blend(green), blend(blue))
}

#[allow(clippy::too_many_lines)]
pub fn paint_track_viewport_chrome(
    ui: &mut Ui,
    geometry: CanvasGeometry,
    frames: &[TrackFrame],
    annotation_scroll: f32,
    read_scroll: f32,
    scroll_clip_top: f32,
    annotation_clip_bottom: f32,
    selected: Option<TrackFrameKey>,
    selected_alignments: &[TrackId],
) -> TrackGutterResult {
    let mut result = TrackGutterResult::default();
    let painter = ui.painter();
    painter.rect_filled(geometry.gutter, 0.0, READ_PANE_GUTTER_COLOR);
    let annotation_gutter = Rect::from_min_max(
        geometry.gutter.min,
        pos2(
            geometry.gutter.right(),
            annotation_clip_bottom.clamp(geometry.gutter.top(), geometry.gutter.bottom()),
        ),
    );
    if annotation_gutter.height() > 0.0 {
        painter.rect_filled(annotation_gutter, 0.0, ANNOTATION_PANE_GUTTER_COLOR);
    }
    painter.line_segment(
        [geometry.gutter.right_top(), geometry.gutter.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(175)),
    );

    for frame in frames {
        // The primary alignment is represented by one frame in the annotation
        // pane and another in the read pane. Keep every egui widget scoped to
        // its pane: reusing the track key alone gives both headers the same ID,
        // which makes popups and clicks fight each other between frames.
        let read_pane_frame = frame.frozen_bottom.is_some();
        let frame_scroll = if read_pane_frame {
            read_scroll
        } else {
            annotation_scroll
        };
        let (natural_top, bottom) = frame_screen_bounds(frame, frame_scroll);
        let frame_clip_top = if read_pane_frame {
            annotation_clip_bottom
        } else {
            scroll_clip_top
        };
        let visible_top = natural_top.max(frame_clip_top).max(geometry.viewport.top());
        let frame_clip_bottom = frame
            .frozen_bottom
            .map_or(annotation_clip_bottom, |_| geometry.viewport.bottom());
        let visible_bottom = bottom
            .min(frame_clip_bottom)
            .min(geometry.viewport.bottom());
        if visible_bottom <= visible_top {
            continue;
        }
        // Read groups share one scrollable pane. Keep the current sample's
        // header visible until the following group pushes it away, while its
        // read geometry continues to move under the common scrollbar.
        let header_top = if read_pane_frame {
            natural_top.max(frame_clip_top).min(bottom - 22.0)
        } else {
            natural_top
        };
        let label_rect = Rect::from_min_max(
            pos2(geometry.gutter.left(), visible_top),
            pos2(geometry.gutter.right(), visible_bottom),
        );
        let selected_as_part_of_group = match frame.key {
            TrackFrameKey::Alignment(id) => selected_alignments.contains(&id),
            TrackFrameKey::Reference | TrackFrameKey::Auxiliary(_) => false,
        };
        if selected == Some(frame.key) || selected_as_part_of_group {
            // Preserve the pane colour: selection is an accent, not a new row
            // fill that makes adjacent tracks look zebra-striped.
            painter.rect_filled(
                Rect::from_min_max(
                    label_rect.left_top(),
                    pos2(label_rect.left() + 3.0, label_rect.bottom()),
                ),
                0.0,
                Color32::from_rgb(45, 92, 175),
            );
        }
        let raw_header_rect = Rect::from_min_max(
            pos2(label_rect.left(), header_top),
            pos2(label_rect.right(), (header_top + 22.0).min(bottom)),
        );
        let header_rect = raw_header_rect.intersect(label_rect);
        let eye_rect = Rect::from_center_size(
            pos2(raw_header_rect.left() + 11.0, raw_header_rect.center().y),
            egui::vec2(16.0, 16.0),
        );
        let settings_rect = Rect::from_center_size(
            pos2(raw_header_rect.right() - 11.0, raw_header_rect.center().y),
            egui::vec2(18.0, 18.0),
        );
        let response = ui.interact(
            Rect::from_min_max(
                pos2(raw_header_rect.left() + 22.0, raw_header_rect.top()),
                pos2(settings_rect.left(), raw_header_rect.bottom()),
            )
            .intersect(header_rect),
            egui::Id::new(("track-label", frame.key, read_pane_frame)),
            Sense::click_and_drag(),
        );
        let eye = ui.interact(
            eye_rect.intersect(header_rect),
            egui::Id::new(("track-collapse", frame.key, read_pane_frame)),
            Sense::click(),
        );
        let settings = ui.interact(
            settings_rect.intersect(header_rect),
            egui::Id::new(("track-settings", frame.key, read_pane_frame)),
            Sense::click(),
        );
        let is_composite = frame
            .subtracks
            .iter()
            .any(|subtrack| subtrack.composite_member.is_some());
        let frame_eye_action = match (frame.key, read_pane_frame) {
            (TrackFrameKey::Alignment(track_id), true) => {
                TrackUiAction::ToggleAlignmentSubtrack(track_id, AlignmentSubtrack::Reads)
            }
            _ => TrackUiAction::ToggleCollapsed(frame.key),
        };
        eye.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                true,
                !frame.collapsed,
                if read_pane_frame {
                    format!(
                        "{} read content {}",
                        frame.name,
                        if frame.collapsed { "hidden" } else { "visible" }
                    )
                } else if is_composite {
                    format!(
                        "{} composite {}",
                        frame.name,
                        if frame.collapsed { "hidden" } else { "visible" }
                    )
                } else {
                    format!(
                        "{} track content {}",
                        frame.name,
                        if frame.collapsed { "hidden" } else { "visible" }
                    )
                },
            )
        });
        if eye.clicked() {
            result.actions.push(frame_eye_action);
        }
        eye.clone().on_hover_text(if read_pane_frame {
            if frame.collapsed {
                "Show reads".to_owned()
            } else {
                "Hide reads".to_owned()
            }
        } else if is_composite {
            if frame.collapsed {
                format!("Show all layers in {}", frame.name)
            } else {
                format!("Hide all layers in {}", frame.name)
            }
        } else if frame.collapsed {
            format!("Show all content in {}", frame.name)
        } else {
            format!("Hide all content in {}", frame.name)
        });
        settings.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("Open settings for {}", frame.name),
            )
        });
        if settings.clicked() {
            result.actions.push(TrackUiAction::Select(frame.key));
            result.actions.push(TrackUiAction::OpenSettings(frame.key));
        }
        settings.clone().on_hover_text("Track settings");
        if response.clicked() {
            result.actions.push(TrackUiAction::Select(frame.key));
        }
        if response.has_focus() {
            let mut open_settings = false;
            let mut toggle_visibility = false;
            let mut toggle_collapsed = false;
            let mut reorder = 0;
            let mut remove = false;
            ui.input_mut(|input| {
                open_settings = input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                toggle_visibility = input.consume_key(egui::Modifiers::NONE, egui::Key::V);
                toggle_collapsed = input.consume_key(egui::Modifiers::NONE, egui::Key::C);
                if input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp) {
                    reorder = -1;
                } else if input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowDown) {
                    reorder = 1;
                }
                remove = input.consume_key(egui::Modifiers::SHIFT, egui::Key::Delete);
            });
            if open_settings {
                result.actions.push(TrackUiAction::Select(frame.key));
                result.actions.push(TrackUiAction::OpenSettings(frame.key));
            }
            if toggle_visibility {
                result
                    .actions
                    .push(TrackUiAction::ToggleVisibility(frame.key));
            }
            if toggle_collapsed && frame.visible && !matches!(frame.key, TrackFrameKey::Reference) {
                result.actions.push(frame_eye_action);
            }
            if let Some(id) = match frame.key {
                TrackFrameKey::Reference => None,
                TrackFrameKey::Alignment(id) if id.get() == 0 => None,
                TrackFrameKey::Alignment(id) | TrackFrameKey::Auxiliary(id) => Some(id),
            } {
                if reorder != 0 {
                    result
                        .actions
                        .push(TrackUiAction::ReorderKeyboard(id, reorder));
                }
                if remove {
                    result.actions.push(TrackUiAction::Remove(frame.key));
                }
            }
        }
        if response.drag_stopped()
            && let Some(id) = match frame.key {
                TrackFrameKey::Reference => None,
                TrackFrameKey::Alignment(id) if id.get() == 0 => None,
                TrackFrameKey::Alignment(id) | TrackFrameKey::Auxiliary(id) => Some(id),
            }
            && let Some(pointer) = response.interact_pointer_pos()
        {
            result
                .actions
                .push(TrackUiAction::ReorderDropped(id, pointer.y));
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                selected == Some(frame.key) || selected_as_part_of_group,
                format!(
                    "{} {:?} track; Enter opens settings, V toggles visibility, C toggles collapse, Alt plus arrow up or down reorders, Shift plus Delete removes",
                    frame.name, frame.kind
                ),
            )
        });
        if response.has_focus() {
            painter.rect_stroke(
                header_rect.shrink(1.0),
                1.0,
                Stroke::new(2.0, Color32::BLACK),
                egui::StrokeKind::Inside,
            );
        }
        let mut tooltip = format!("{}\n{:?} track", frame.name, frame.kind);
        if let Some(source) = &frame.source_label {
            tooltip.push('\n');
            tooltip.push_str(source);
        }
        for subtrack in &frame.subtracks {
            tooltip.push('\n');
            tooltip.push_str(&subtrack.label);
            if let Some(detail) = &subtrack.detail {
                tooltip.push_str(": ");
                tooltip.push_str(detail);
            }
        }
        response.clone().on_hover_text(tooltip);
        response.context_menu(|ui| {
            if ui.button("Track settings…").clicked() {
                result.actions.push(TrackUiAction::OpenSettings(frame.key));
                ui.close();
            }
            if read_pane_frame
                && let TrackFrameKey::Alignment(track_id) = frame.key
                && ui
                    .button(if frame.collapsed {
                        "Show reads"
                    } else {
                        "Hide reads"
                    })
                    .clicked()
            {
                result.actions.push(TrackUiAction::ToggleAlignmentSubtrack(
                    track_id,
                    AlignmentSubtrack::Reads,
                ));
                ui.close();
            }
            if !read_pane_frame
                && !matches!(frame.key, TrackFrameKey::Reference)
                && ui
                    .button(if frame.visible {
                        "Hide track"
                    } else {
                        "Show track"
                    })
                    .clicked()
            {
                result
                    .actions
                    .push(TrackUiAction::ToggleVisibility(frame.key));
                ui.close();
            }
            if !read_pane_frame
                && frame.visible
                && ui
                    .button(if is_composite {
                        if frame.collapsed {
                            "Show all overlay layers"
                        } else {
                            "Hide all overlay layers"
                        }
                    } else if frame.collapsed {
                        "Expand track"
                    } else {
                        "Collapse track"
                    })
                    .clicked()
            {
                result
                    .actions
                    .push(TrackUiAction::ToggleCollapsed(frame.key));
                ui.close();
            }
            if ui.button("Export current view…").clicked() {
                result.actions.push(TrackUiAction::Export(frame.key));
                ui.close();
            }
            if !matches!(frame.key, TrackFrameKey::Reference)
                && !matches!(frame.key, TrackFrameKey::Alignment(id) if id.get() == 0)
                && ui.button("Remove track").clicked()
            {
                result.actions.push(TrackUiAction::Remove(frame.key));
                ui.close();
            }
        });
        let text_color = if frame.visible {
            Color32::from_gray(45)
        } else {
            Color32::from_gray(145)
        };
        let label_painter = painter.with_clip_rect(label_rect.shrink2(egui::vec2(3.0, 1.0)));
        let eye_color = if eye.hovered() {
            Color32::from_rgb(45, 92, 175)
        } else {
            Color32::from_gray(105)
        };
        label_painter.add(egui::epaint::Shape::ellipse_stroke(
            eye_rect.center(),
            egui::vec2(5.0, 3.2),
            Stroke::new(1.0, eye_color),
        ));
        if frame.collapsed {
            label_painter.line_segment(
                [
                    eye_rect.left_top() + egui::vec2(2.5, 2.5),
                    eye_rect.right_bottom() - egui::vec2(2.5, 2.5),
                ],
                Stroke::new(1.4, eye_color),
            );
        } else {
            label_painter.circle_filled(eye_rect.center(), 1.6, eye_color);
        }
        if settings.hovered() {
            label_painter.rect_filled(
                settings_rect,
                3.0,
                Color32::from_rgba_unmultiplied(45, 92, 175, 24),
            );
        }
        label_painter.text(
            settings_rect.center(),
            Align2::CENTER_CENTER,
            "⚙",
            FontId::proportional(13.0),
            if settings.hovered() {
                Color32::from_rgb(45, 92, 175)
            } else {
                Color32::from_gray(95)
            },
        );
        paint_elided_text(
            &label_painter,
            &frame.name,
            FontId::proportional(11.0),
            text_color,
            (label_rect.width() - 53.0).max(0.0),
            pos2(label_rect.left() + 22.0, header_top + 11.0),
            false,
        );
        for subtrack in &frame.subtracks {
            let (raw_subtrack_top, raw_subtrack_bottom) =
                subtrack_screen_bounds(frame, subtrack, frame_scroll);
            let subtrack_clip_top = frame_clip_top;
            let subtrack_top = raw_subtrack_top
                .max(header_top + 19.0)
                .max(subtrack_clip_top)
                .max(geometry.viewport.top());
            let subtrack_bottom = raw_subtrack_bottom
                .min(label_rect.bottom())
                .min(frame_clip_bottom)
                .min(geometry.viewport.bottom());
            if subtrack_bottom <= subtrack_top {
                continue;
            }
            if frame.key == TrackFrameKey::Reference
                && let Some((accessible_name, tooltip)) =
                    reference_orientation_accessibility(&subtrack.label)
            {
                let orientation = ui.interact(
                    Rect::from_min_max(
                        pos2(label_rect.left(), subtrack_top),
                        pos2(label_rect.right(), subtrack_bottom),
                    ),
                    egui::Id::new("reference-sequence-orientation"),
                    Sense::hover(),
                );
                orientation.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Label, true, accessible_name)
                });
                orientation.on_hover_text(tooltip);
            }
            let y = (raw_subtrack_top + 9.0).min(raw_subtrack_bottom - 2.0);
            if y < subtrack_top || y >= subtrack_bottom {
                continue;
            }
            let alignment_subtrack = match frame.key {
                TrackFrameKey::Alignment(_) => AlignmentSubtrack::from_label(&subtrack.label),
                TrackFrameKey::Reference | TrackFrameKey::Auxiliary(_) => None,
            };
            if let (TrackFrameKey::Alignment(track_id), Some(alignment_subtrack)) =
                (frame.key, alignment_subtrack)
            {
                if subtrack.collapsed {
                    painter.rect_filled(
                        Rect::from_min_max(
                            pos2(geometry.gutter.left(), subtrack_top),
                            pos2(geometry.gutter.right(), subtrack_bottom),
                        ),
                        0.0,
                        Color32::from_gray(242),
                    );
                    painter.line_segment(
                        [
                            pos2(geometry.gutter.left(), subtrack_bottom),
                            pos2(geometry.gutter.right(), subtrack_bottom),
                        ],
                        Stroke::new(1.0, Color32::from_gray(218)),
                    );
                }
                let eye_rect = Rect::from_center_size(
                    pos2(label_rect.left() + 11.0, y),
                    egui::vec2(16.0, 16.0),
                );
                let eye = ui.interact(
                    eye_rect.intersect(Rect::from_min_max(
                        pos2(label_rect.left(), subtrack_top),
                        pos2(label_rect.right(), subtrack_bottom),
                    )),
                    egui::Id::new((
                        "alignment-subtrack-visibility",
                        track_id,
                        alignment_subtrack,
                        read_pane_frame,
                    )),
                    Sense::click(),
                );
                eye.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Checkbox,
                        true,
                        !subtrack.collapsed,
                        format!(
                            "{} subtrack {}",
                            alignment_subtrack.label(),
                            if subtrack.collapsed {
                                "hidden"
                            } else {
                                "visible"
                            }
                        ),
                    )
                });
                if eye.clicked() {
                    result.actions.push(TrackUiAction::ToggleAlignmentSubtrack(
                        track_id,
                        alignment_subtrack,
                    ));
                }
                let visibility_action = if subtrack.collapsed { "Show" } else { "Hide" };
                eye.clone().on_hover_text(format!(
                    "{visibility_action} {} subtrack",
                    alignment_subtrack.label()
                ));
                eye.context_menu(|ui| {
                    if ui
                        .button(format!(
                            "{visibility_action} {}",
                            alignment_subtrack.label()
                        ))
                        .clicked()
                    {
                        result.actions.push(TrackUiAction::ToggleAlignmentSubtrack(
                            track_id,
                            alignment_subtrack,
                        ));
                        ui.close();
                    }
                });
                let eye_color = if eye.hovered() {
                    Color32::from_rgb(45, 92, 175)
                } else {
                    Color32::from_gray(115)
                };
                label_painter.add(egui::epaint::Shape::ellipse_stroke(
                    eye_rect.center(),
                    egui::vec2(5.0, 3.2),
                    Stroke::new(1.0, eye_color),
                ));
                if subtrack.collapsed {
                    label_painter.line_segment(
                        [
                            eye_rect.left_top() + egui::vec2(2.5, 2.5),
                            eye_rect.right_bottom() - egui::vec2(2.5, 2.5),
                        ],
                        Stroke::new(1.4, eye_color),
                    );
                } else {
                    label_painter.circle_filled(eye_rect.center(), 1.6, eye_color);
                }
            }
            if let Some(member_id) = subtrack.composite_member {
                if subtrack.collapsed {
                    painter.rect_filled(
                        Rect::from_min_max(
                            pos2(geometry.gutter.left(), subtrack_top),
                            pos2(geometry.gutter.right(), subtrack_bottom),
                        ),
                        0.0,
                        Color32::from_gray(242),
                    );
                }
                let eye_rect = Rect::from_center_size(
                    pos2(label_rect.left() + 11.0, y),
                    egui::vec2(16.0, 16.0),
                );
                let eye = ui.interact(
                    eye_rect.intersect(Rect::from_min_max(
                        pos2(label_rect.left(), subtrack_top),
                        pos2(label_rect.right(), subtrack_bottom),
                    )),
                    egui::Id::new(("composite-member-visibility", member_id, read_pane_frame)),
                    Sense::click(),
                );
                eye.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Checkbox,
                        true,
                        !subtrack.collapsed,
                        format!(
                            "{} composite member {}",
                            subtrack.label,
                            if subtrack.collapsed {
                                "hidden"
                            } else {
                                "visible"
                            }
                        ),
                    )
                });
                if eye.clicked() {
                    result
                        .actions
                        .push(TrackUiAction::ToggleCompositeMember(member_id));
                }
                let visibility_action = if subtrack.collapsed { "Show" } else { "Hide" };
                eye.clone().on_hover_text(format!(
                    "{visibility_action} {} in this overlay",
                    subtrack.label
                ));
                eye.context_menu(|ui| {
                    if ui
                        .button(format!("{visibility_action} {}", subtrack.label))
                        .clicked()
                    {
                        result
                            .actions
                            .push(TrackUiAction::ToggleCompositeMember(member_id));
                        ui.close();
                    }
                });
                let eye_color = if eye.hovered() {
                    Color32::from_rgb(45, 92, 175)
                } else {
                    Color32::from_gray(115)
                };
                label_painter.add(egui::epaint::Shape::ellipse_stroke(
                    eye_rect.center(),
                    egui::vec2(5.0, 3.2),
                    Stroke::new(1.0, eye_color),
                ));
                if subtrack.collapsed {
                    label_painter.line_segment(
                        [
                            eye_rect.left_top() + egui::vec2(2.5, 2.5),
                            eye_rect.right_bottom() - egui::vec2(2.5, 2.5),
                        ],
                        Stroke::new(1.4, eye_color),
                    );
                } else {
                    label_painter.circle_filled(eye_rect.center(), 1.6, eye_color);
                }
            }
            let label_font = FontId::proportional(10.0);
            let detail_font = FontId::proportional(9.0);
            let label_color = Color32::from_gray(if subtrack.collapsed { 115 } else { 75 });
            let detail_color = Color32::from_gray(if subtrack.collapsed { 135 } else { 105 });
            let label_width = label_painter
                .layout_no_wrap(subtrack.label.clone(), label_font.clone(), label_color)
                .size()
                .x;
            let detail_width = subtrack.detail.as_ref().map_or(0.0, |detail| {
                label_painter
                    .layout_no_wrap(detail.clone(), detail_font.clone(), detail_color)
                    .size()
                    .x
            });
            let available_width = (label_rect.width() - 29.0).max(0.0);
            let (label_width, detail_width) =
                gutter_row_widths(label_width, detail_width, available_width);
            paint_elided_text(
                &label_painter,
                &subtrack.label,
                label_font,
                label_color,
                label_width,
                pos2(label_rect.left() + 22.0, y),
                false,
            );
            if let Some(detail) = &subtrack.detail {
                paint_elided_text(
                    &label_painter,
                    detail,
                    detail_font,
                    detail_color,
                    detail_width,
                    pos2(label_rect.right() - 7.0, y),
                    true,
                );
            }
        }
        if boundary_inside_clip(bottom, frame_clip_top, frame_clip_bottom) {
            painter.line_segment(
                [
                    pos2(geometry.viewport.left(), bottom),
                    pos2(geometry.viewport.right(), bottom),
                ],
                Stroke::new(1.0, Color32::from_gray(205)),
            );
        }

        if frame.collapsed {
            continue;
        }
        if !boundary_inside_clip(bottom, frame_clip_top, frame_clip_bottom) {
            continue;
        }
        let handle = Rect::from_min_max(
            pos2(geometry.gutter.left(), bottom - 3.0),
            pos2(geometry.gutter.right(), bottom + 3.0),
        )
        .intersect(Rect::from_min_max(
            pos2(geometry.gutter.left(), frame_clip_top),
            pos2(geometry.gutter.right(), frame_clip_bottom),
        ));
        let resize = ui
            .interact(
                handle,
                egui::Id::new(("track-resize", frame.key, read_pane_frame)),
                Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeVertical);
        resize.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::ResizeHandle,
                true,
                format!(
                    "Resize {} track; arrow up or down changes height",
                    frame.name
                ),
            )
        });
        if resize.has_focus() {
            let delta = ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    -10.0
                } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    10.0
                } else {
                    0.0
                }
            });
            if delta != 0.0 {
                if read_pane_frame {
                    let TrackFrameKey::Alignment(id) = frame.key else {
                        continue;
                    };
                    result.actions.push(TrackUiAction::ResizeReadTrackKeyboard(
                        id,
                        frame.bottom - frame.top,
                        delta,
                    ));
                } else {
                    match frame.key {
                        TrackFrameKey::Auxiliary(id) => {
                            result
                                .actions
                                .push(TrackUiAction::ResizeKeyboard(id, delta));
                        }
                        TrackFrameKey::Reference | TrackFrameKey::Alignment(_) => {
                            result.actions.push(TrackUiAction::ResizeAnnotationKeyboard(
                                frame.key,
                                frame.bottom - frame.top,
                                delta,
                            ));
                        }
                    }
                }
            }
            painter.line_segment(
                [handle.left_center(), handle.right_center()],
                Stroke::new(2.0, Color32::BLACK),
            );
        }
        if resize.drag_started() {
            if read_pane_frame {
                let TrackFrameKey::Alignment(id) = frame.key else {
                    continue;
                };
                result.actions.push(TrackUiAction::ResizeReadTrackStarted(
                    id,
                    frame.bottom - frame.top,
                ));
            } else {
                match frame.key {
                    TrackFrameKey::Auxiliary(id) => {
                        result.actions.push(TrackUiAction::ResizeStarted(id));
                    }
                    TrackFrameKey::Reference | TrackFrameKey::Alignment(_) => {
                        result.actions.push(TrackUiAction::ResizeAnnotationStarted(
                            frame.key,
                            frame.bottom - frame.top,
                        ));
                    }
                }
            }
        }
        if resize.dragged() {
            if read_pane_frame {
                let TrackFrameKey::Alignment(id) = frame.key else {
                    continue;
                };
                let delta = resize.drag_delta().y;
                if delta != 0.0 {
                    result
                        .actions
                        .push(TrackUiAction::ResizeReadTrackDragged(id, delta));
                }
            } else {
                match frame.key {
                    TrackFrameKey::Auxiliary(id) => result
                        .actions
                        .push(TrackUiAction::ResizeDragged(id, resize.drag_delta().y)),
                    TrackFrameKey::Reference | TrackFrameKey::Alignment(_) => result.actions.push(
                        TrackUiAction::ResizeAnnotationDragged(frame.key, resize.drag_delta().y),
                    ),
                }
            }
        }
        if resize.drag_stopped() {
            if read_pane_frame {
                let TrackFrameKey::Alignment(id) = frame.key else {
                    continue;
                };
                result
                    .actions
                    .push(TrackUiAction::ResizeReadTrackStopped(id));
            } else {
                match frame.key {
                    TrackFrameKey::Auxiliary(id) => {
                        result.actions.push(TrackUiAction::ResizeStopped(id));
                    }
                    TrackFrameKey::Reference | TrackFrameKey::Alignment(_) => result
                        .actions
                        .push(TrackUiAction::ResizeAnnotationStopped(frame.key)),
                }
            }
        }
    }
    if annotation_clip_bottom > geometry.viewport.top()
        && annotation_clip_bottom < geometry.viewport.bottom()
    {
        let divider_handle = Rect::from_min_max(
            pos2(geometry.gutter.left(), annotation_clip_bottom - 4.0),
            pos2(geometry.scrollbar.left(), annotation_clip_bottom + 4.0),
        );
        let divider = ui
            .interact(
                divider_handle,
                egui::Id::new("annotation-read-pane-divider"),
                Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text("Resize annotation and read panes");
        divider.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::ResizeHandle,
                true,
                "Resize annotation and read panes; arrow up enlarges reads and arrow down enlarges annotations",
            )
        });
        if divider.has_focus() {
            let delta = ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    -10.0
                } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    10.0
                } else {
                    0.0
                }
            });
            if delta != 0.0 {
                result
                    .actions
                    .push(TrackUiAction::ResizeReadPaneKeyboard(delta));
            }
        }
        if divider.drag_started() {
            result.actions.push(TrackUiAction::ResizeReadPaneStarted);
        }
        if divider.dragged() {
            let delta = divider.drag_delta().y;
            if delta != 0.0 {
                result
                    .actions
                    .push(TrackUiAction::ResizeReadPaneDragged(delta));
            }
        }
        if divider.drag_stopped() {
            result.actions.push(TrackUiAction::ResizeReadPaneStopped);
        }
        painter.line_segment(
            [
                pos2(geometry.viewport.left(), annotation_clip_bottom),
                pos2(geometry.viewport.right(), annotation_clip_bottom),
            ],
            Stroke::new(1.5, Color32::from_gray(165)),
        );
    }
    result
}

fn boundary_inside_clip(boundary: f32, clip_top: f32, clip_bottom: f32) -> bool {
    boundary >= clip_top && boundary <= clip_bottom
}

fn frame_screen_bounds(frame: &TrackFrame, vertical_scroll: f32) -> (f32, f32) {
    (frame.top - vertical_scroll, frame.bottom - vertical_scroll)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct VerticalScreenBounds {
    top: f32,
    bottom: f32,
}

fn clipped_frame_screen_bounds(
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

fn reference_orientation_accessibility(label: &str) -> Option<(&'static str, &'static str)> {
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

fn subtrack_screen_bounds(
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
        let color = Color32::from_rgba_unmultiplied(
            (text.color.red.clamp(0.0, 1.0) * 255.0).round() as u8,
            (text.color.green.clamp(0.0, 1.0) * 255.0).round() as u8,
            (text.color.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
            (text.color.alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
        );
        painter.text(
            pos2(text.x, text.y),
            Align2::CENTER_CENTER,
            &text.text,
            FontId::monospace(text.font_size),
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_boundaries_cannot_enter_the_read_pane() {
        assert!(boundary_inside_clip(240.0, 100.0, 300.0));
        assert!(boundary_inside_clip(300.0, 100.0, 300.0));
        assert!(!boundary_inside_clip(301.0, 100.0, 300.0));
        assert!(!boundary_inside_clip(480.0, 100.0, 300.0));
    }

    #[test]
    fn reference_orientation_indicator_has_accessible_names_and_tooltips() {
        assert_eq!(
            reference_orientation_accessibility("> Bases · forward"),
            Some((
                "Reference sequence, forward genomic direction",
                "Reference sequence is displayed in the forward genomic direction"
            ))
        );
        assert_eq!(
            reference_orientation_accessibility("< Bases · reverse strand"),
            Some((
                "Reference sequence, reverse-complement direction",
                "Reference sequence is displayed as the reverse complement"
            ))
        );
        assert_eq!(reference_orientation_accessibility("Bases"), None);
    }

    #[test]
    fn alignment_subtrack_labels_round_trip_to_gutter_actions() {
        for subtrack in AlignmentSubtrack::ALL {
            assert_eq!(
                AlignmentSubtrack::from_label(subtrack.label()),
                Some(subtrack)
            );
        }
        assert_eq!(AlignmentSubtrack::from_label("Genes"), None);
    }

    fn frame(id: u64, top: f32) -> TrackFrame {
        TrackFrame {
            key: TrackFrameKey::Auxiliary(TrackId::new(id)),
            name: format!("track-{id}"),
            kind: TrackKind::Feature,
            source_label: Some(format!("track-{id}.bed")),
            top,
            bottom: top + 100.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        }
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1.0e-3,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn gutter_row_widths_keep_label_and_detail_apart() {
        let available_width = 143.0;
        let (label_width, detail_width) = gutter_row_widths(62.0, 240.0, available_width);

        assert_close(label_width, 62.0);
        assert_close(
            label_width + GUTTER_TEXT_GAP + detail_width,
            available_width,
        );
        assert!(detail_width > 0.0);
    }

    #[test]
    fn gutter_text_is_elided_to_one_bounded_row() {
        let context = egui::Context::default();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let galley = layout_elided_text(
                ui.painter(),
                "2 types · 5 tagged reads · 0 malformed · 0 unavailable",
                FontId::proportional(9.0),
                Color32::from_gray(105),
                70.0,
            );

            assert!(galley.elided);
            assert_eq!(galley.rows.len(), 1);
            assert!(galley.size().x <= 70.0);
        });
    }

    #[test]
    fn drop_target_accounts_for_vertical_scroll() {
        let frames = [frame(1, 200.0), frame(2, 300.0), frame(3, 400.0)];
        let order = [TrackId::new(1), TrackId::new(2), TrackId::new(3)];
        assert_eq!(track_drop_index(&frames, &order, 220.0, 100.0, 0.0), 1);
        assert_eq!(track_drop_index(&frames, &order, 500.0, 100.0, 0.0), 2);
    }

    #[test]
    fn keyboard_reorder_stops_at_both_track_boundaries() {
        assert_eq!(keyboard_reorder_index(0, 4, -1), 0);
        assert_eq!(keyboard_reorder_index(1, 4, -1), 0);
        assert_eq!(keyboard_reorder_index(1, 4, 1), 2);
        assert_eq!(keyboard_reorder_index(3, 4, 1), 3);
    }

    #[test]
    fn scrollbar_overview_preserves_empty_and_dense_content_regions() {
        let frames = [TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(1)),
            name: "reads".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 100.0,
            bottom: 500.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: Vec::new(),
        }];
        let display = DisplayList {
            rectangles: (0..24)
                .map(|index| rgv_layout::RectPrimitive {
                    x: 0.0,
                    y: 300.0 + index as f32,
                    width: 80.0,
                    height: 3.0,
                    color: rgv_layout::Rgba::new(0.2, 0.4, 0.8, 1.0),
                    object_id: index,
                    shape: Default::default(),
                })
                .collect(),
            texts: Vec::new(),
        };

        let overview = scrollbar_overview(&frames, &display, 100.0, 500.0, 100.0, 0.0, 100.0, 40);
        assert_eq!(overview.bins.len(), 40);
        assert!(overview.bins[..15].iter().all(|bin| bin.density == 0.0));
        assert!(overview.bins[20..25].iter().any(|bin| bin.density > 0.5));
        assert!(
            overview
                .bins
                .iter()
                .all(|bin| bin.kind == Some(TrackKind::Alignment))
        );
        assert_eq!(overview.boundaries, vec![0.0]);
    }

    #[test]
    fn scrollbar_overview_places_activity_in_the_density_camera_world() {
        let frames = [TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(1)),
            name: "reads".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 100.0,
            bottom: 500.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(200.0),
            subtracks: Vec::new(),
        }];
        let display = DisplayList {
            rectangles: vec![rgv_layout::RectPrimitive {
                x: 0.0,
                y: 220.0,
                width: 80.0,
                height: 10.0,
                color: rgv_layout::Rgba::new(0.2, 0.4, 0.8, 1.0),
                object_id: 1,
                shape: Default::default(),
            }],
            texts: Vec::new(),
        };

        let overview = scrollbar_overview(&frames, &display, 100.0, 900.0, 200.0, 300.0, 100.0, 80);
        assert!(overview.bins[40..45].iter().any(|bin| bin.density > 0.0));
        assert!(overview.bins[..35].iter().all(|bin| bin.density == 0.0));
        assert_eq!(overview.bins[42].kind, Some(TrackKind::Alignment));
    }

    #[test]
    fn custom_vertical_overview_exposes_a_named_accessibility_slider() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(100.0, 500.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let mut offset = 250.0;
                let _ = paint_vertical_scrollbar(
                    ui,
                    Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(22.0, 400.0)),
                    1_000.0,
                    &mut offset,
                    1.0,
                    400.0,
                    None,
                    None,
                );
                let mut annotation_offset = 500.0;
                let _ = paint_annotation_scrollbar(
                    ui,
                    Rect::from_min_size(egui::pos2(40.0, 10.0), egui::vec2(22.0, 400.0)),
                    1_000.0,
                    &mut annotation_offset,
                    None,
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("Read stack overview"))
                && node.numeric_value() == Some(25.0)
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("Annotation track scrollbar"))
                && node.numeric_value() == Some(50.0)
        }));
    }

    #[test]
    fn vertical_overview_reports_press_before_egui_drag_threshold() {
        let context = egui::Context::default();
        let pointer = egui::pos2(20.0, 100.0);
        let mut interaction = ScrollbarInteraction::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 500.0));
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let mut offset = 250.0;
                let _ = paint_vertical_scrollbar(
                    ui,
                    Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(22.0, 400.0)),
                    1_000.0,
                    &mut offset,
                    1.0,
                    400.0,
                    None,
                    None,
                );
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| {
                let mut offset = 250.0;
                interaction = paint_vertical_scrollbar(
                    ui,
                    Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(22.0, 400.0)),
                    1_000.0,
                    &mut offset,
                    1.0,
                    400.0,
                    None,
                    None,
                );
            },
        );

        assert!(interaction.pointer_down);
        assert!(!interaction.drag_started);
        assert!(!interaction.dragging);
    }

    #[test]
    fn custom_track_gutter_exposes_named_selection_and_resize_controls() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 500.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let geometry = CanvasGeometry::from_viewport(ui.max_rect());
                let frames = [frame(7, 20.0)];
                let _ = paint_track_viewport_chrome(
                    ui,
                    geometry,
                    &frames,
                    0.0,
                    0.0,
                    geometry.viewport.top(),
                    geometry.viewport.bottom(),
                    Some(TrackFrameKey::Auxiliary(TrackId::new(7))),
                    &[],
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("track-7 Feature track"))
                && node.toggled() == Some(egui::accesskit::Toggled::True)
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Splitter
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("Resize track-7 track"))
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::CheckBox
                && node
                    .label()
                    .is_some_and(|label| label == "track-7 track content visible")
                && node.toggled() == Some(egui::accesskit::Toggled::True)
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node
                    .label()
                    .is_some_and(|label| label == "Open settings for track-7")
        }));
    }

    #[test]
    fn duplicated_primary_alignment_frames_have_distinct_header_controls() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 500.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let track_id = TrackId::new(9);
                let frames = [
                    TrackFrame {
                        key: TrackFrameKey::Alignment(track_id),
                        name: "exact-bases.bam".to_owned(),
                        kind: TrackKind::Alignment,
                        source_label: None,
                        top: 20.0,
                        bottom: 120.0,
                        visible: true,
                        collapsed: false,
                        frozen_bottom: None,
                        subtracks: Vec::new(),
                    },
                    TrackFrame {
                        key: TrackFrameKey::Alignment(track_id),
                        name: "Reads · exact-bases.bam".to_owned(),
                        kind: TrackKind::Alignment,
                        source_label: None,
                        top: 180.0,
                        bottom: 420.0,
                        visible: true,
                        collapsed: false,
                        frozen_bottom: Some(202.0),
                        subtracks: Vec::new(),
                    },
                ];
                let geometry = CanvasGeometry::from_viewport(ui.max_rect());
                let _ = paint_track_viewport_chrome(
                    ui,
                    geometry,
                    &frames,
                    0.0,
                    0.0,
                    0.0,
                    180.0,
                    None,
                    &[],
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let settings_buttons = update
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.role() == egui::accesskit::Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Open settings for "))
            })
            .count();
        let visibility_eyes = update
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.role() == egui::accesskit::Role::CheckBox
                    && node
                        .label()
                        .is_some_and(|label| label.contains("content visible"))
            })
            .count();

        assert_eq!(settings_buttons, 2);
        assert_eq!(visibility_eyes, 2);
    }

    #[test]
    fn duplicated_primary_alignment_header_context_menu_stays_open() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [
            TrackFrame {
                key: TrackFrameKey::Alignment(track_id),
                name: "exact-bases.bam".to_owned(),
                kind: TrackKind::Alignment,
                source_label: None,
                top: 20.0,
                bottom: 120.0,
                visible: true,
                collapsed: false,
                frozen_bottom: None,
                subtracks: Vec::new(),
            },
            TrackFrame {
                key: TrackFrameKey::Alignment(track_id),
                name: "Reads · exact-bases.bam".to_owned(),
                kind: TrackKind::Alignment,
                source_label: None,
                top: 180.0,
                bottom: 420.0,
                visible: true,
                collapsed: false,
                frozen_bottom: Some(202.0),
                subtracks: Vec::new(),
            },
        ];
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                180.0,
                None,
                &[],
            )
        };
        let pointer = pos2(50.0, 31.0);

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Secondary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Secondary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let popup_id =
            egui::Id::new(("track-label", TrackFrameKey::Alignment(track_id), false)).with("popup");
        assert!(egui::Popup::is_id_open(&context, popup_id));

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        assert!(egui::Popup::is_id_open(&context, popup_id));
    }

    #[test]
    fn clicking_visible_track_settings_button_opens_settings() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let frames = [frame(7, 20.0)];
        let pointer = pos2(157.0, 31.0);
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                ui.max_rect().bottom(),
                None,
                &[],
            )
        };

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let mut actions = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..egui::RawInput::default()
            },
            |ui| {
                actions = render(ui).actions;
            },
        );

        assert_eq!(
            actions,
            vec![
                TrackUiAction::Select(TrackFrameKey::Auxiliary(TrackId::new(7))),
                TrackUiAction::OpenSettings(TrackFrameKey::Auxiliary(TrackId::new(7))),
            ]
        );
    }

    #[test]
    fn clicking_a_track_eye_requests_collapse() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let frames = [frame(7, 20.0)];
        let pointer = pos2(11.0, 31.0);
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                ui.max_rect().bottom(),
                None,
                &[],
            )
        };

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let mut actions = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..egui::RawInput::default()
            },
            |ui| {
                actions = render(ui).actions;
            },
        );

        assert_eq!(
            actions,
            vec![TrackUiAction::ToggleCollapsed(TrackFrameKey::Auxiliary(
                TrackId::new(7),
            ))]
        );
    }

    #[test]
    fn clicking_read_pane_eye_toggles_reads_instead_of_the_whole_track() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            name: "Reads · exact-bases.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 180.0,
            bottom: 420.0,
            visible: true,
            collapsed: true,
            frozen_bottom: Some(202.0),
            subtracks: Vec::new(),
        }];
        let pointer = pos2(11.0, 191.0);
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                180.0,
                None,
                &[],
            )
        };

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let mut actions = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..egui::RawInput::default()
            },
            |ui| {
                actions = render(ui).actions;
            },
        );

        assert_eq!(
            actions,
            vec![TrackUiAction::ToggleAlignmentSubtrack(
                track_id,
                AlignmentSubtrack::Reads,
            )]
        );
    }

    #[test]
    fn hidden_alignment_subtrack_keeps_a_collapsed_closed_eye_row() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 500.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let track_id = TrackId::new(9);
                let geometry = CanvasGeometry::from_viewport(ui.max_rect());
                let frames = [TrackFrame {
                    key: TrackFrameKey::Alignment(track_id),
                    name: "reads.bam".to_owned(),
                    kind: TrackKind::Alignment,
                    source_label: None,
                    top: 20.0,
                    bottom: 300.0,
                    visible: true,
                    collapsed: false,
                    frozen_bottom: None,
                    subtracks: vec![TrackSubtrack {
                        label: "Coverage".to_owned(),
                        detail: None,
                        top: 42.0,
                        bottom: 62.0,
                        collapsed: true,
                        composite_member: None,
                    }],
                }];
                let _ = paint_track_viewport_chrome(
                    ui,
                    geometry,
                    &frames,
                    0.0,
                    0.0,
                    0.0,
                    geometry.viewport.bottom(),
                    None,
                    &[],
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::CheckBox
                && node
                    .label()
                    .is_some_and(|label| label == "Coverage subtrack hidden")
                && node.toggled() == Some(egui::accesskit::Toggled::False)
        }));
    }

    #[test]
    fn clicking_closed_eye_on_collapsed_subtrack_requests_reenable() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            name: "reads.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 20.0,
            bottom: 300.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: vec![TrackSubtrack {
                label: "Coverage".to_owned(),
                detail: None,
                top: 42.0,
                bottom: 62.0,
                collapsed: true,
                composite_member: None,
            }],
        }];
        let pointer = pos2(11.0, 51.0);
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                ui.max_rect().bottom(),
                None,
                &[],
            )
        };

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let mut actions = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..egui::RawInput::default()
            },
            |ui| {
                actions = render(ui).actions;
            },
        );

        assert_eq!(
            actions,
            vec![TrackUiAction::ToggleAlignmentSubtrack(
                track_id,
                AlignmentSubtrack::Coverage,
            )]
        );
    }

    #[test]
    fn clicking_composite_member_eye_targets_only_that_member() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let member_id = TrackId::new(9);
        let frames = [TrackFrame {
            key: TrackFrameKey::Auxiliary(TrackId::new(7)),
            name: "Overlay 1 · 3 tracks".to_owned(),
            kind: TrackKind::Quantitative,
            source_label: None,
            top: 20.0,
            bottom: 140.0,
            visible: true,
            collapsed: false,
            frozen_bottom: None,
            subtracks: vec![TrackSubtrack {
                label: "normal".to_owned(),
                detail: Some("overlay 1".to_owned()),
                top: 42.0,
                bottom: 62.0,
                collapsed: false,
                composite_member: Some(member_id),
            }],
        }];
        let pointer = pos2(11.0, 51.0);
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                ui.max_rect().bottom(),
                None,
                &[],
            )
        };

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let mut actions = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..egui::RawInput::default()
            },
            |ui| actions = render(ui).actions,
        );

        assert_eq!(
            actions,
            vec![TrackUiAction::ToggleCompositeMember(member_id)]
        );
    }

    #[test]
    fn resize_clamps_to_track_height_constraints() {
        assert_close(resized_track_height(120.0, 30.0), 150.0);
        assert_close(resized_track_height(120.0, -500.0), 40.0);
        assert_close(resized_track_height(120.0, 900.0), 600.0);
        assert_close(resized_read_track_height(120.0, 30.0), 150.0);
        assert_close(resized_read_track_height(120.0, -500.0), 40.0);
        assert_close(resized_read_track_height(120.0, 2_000.0), 1_200.0);
    }

    #[test]
    fn read_group_boundary_is_resizable_from_the_left_gutter() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            name: "Reads · sample A".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 200.0,
            bottom: 300.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(222.0),
            subtracks: vec![TrackSubtrack {
                label: "Reads".to_owned(),
                detail: None,
                top: 222.0,
                bottom: 300.0,
                collapsed: false,
                composite_member: None,
            }],
        }];
        let pointer = pos2(50.0, 300.0);
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                200.0,
                None,
                &[],
            )
        };
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let mut actions = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| actions = render(ui).actions,
        );

        assert_eq!(
            actions,
            vec![TrackUiAction::ResizeReadTrackStarted(track_id, 100.0)]
        );
    }

    #[test]
    fn incremental_track_resize_deltas_accumulate() {
        let after_first_frame = resized_track_height(120.0, 10.0);
        let after_second_frame = resized_track_height(after_first_frame, 10.0);
        assert_close(after_second_frame, 140.0);
    }

    #[test]
    fn pane_resize_moves_the_divider_and_preserves_both_minimums() {
        assert_close(resized_read_pane_height(360.0, -30.0, 800.0), 390.0);
        assert_close(resized_read_pane_height(360.0, 30.0, 800.0), 330.0);
        assert_close(resized_read_pane_height(120.0, 200.0, 800.0), 100.0);
        assert_close(resized_read_pane_height(700.0, -200.0, 800.0), 720.0);
    }

    #[test]
    fn pane_divider_reports_incremental_drag_actions() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &[],
                0.0,
                0.0,
                0.0,
                250.0,
                None,
                &[],
            )
        };
        // The pane divider must be draggable from the left gutter as well as
        // from the genomic canvas.
        let pointer = pos2(50.0, 250.0);
        let mut actions = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..egui::RawInput::default()
            },
            |ui| actions = render(ui).actions,
        );
        assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneStarted]);
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerMoved(pos2(50.0, 260.0))],
                ..egui::RawInput::default()
            },
            |ui| actions = render(ui).actions,
        );
        assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneDragged(10.0)]);

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerMoved(pos2(50.0, 270.0))],
                ..egui::RawInput::default()
            },
            |ui| actions = render(ui).actions,
        );
        assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneDragged(10.0)]);
    }

    #[test]
    fn alignment_read_groups_follow_the_shared_read_scroll() {
        let frame = TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(1)),
            name: "reads.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 100.0,
            bottom: 800.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(260.0),
            subtracks: vec![
                TrackSubtrack {
                    label: "Reference".to_owned(),
                    detail: None,
                    top: 125.0,
                    bottom: 165.0,
                    collapsed: false,
                    composite_member: None,
                },
                TrackSubtrack {
                    label: "Reads".to_owned(),
                    detail: None,
                    top: 260.0,
                    bottom: 800.0,
                    collapsed: false,
                    composite_member: None,
                },
            ],
        };

        assert_eq!(frame_screen_bounds(&frame, 300.0), (-200.0, 500.0));
        assert_eq!(
            subtrack_screen_bounds(&frame, &frame.subtracks[0], 300.0),
            (-175.0, -135.0)
        );
        assert_eq!(
            subtrack_screen_bounds(&frame, &frame.subtracks[1], 300.0),
            (-40.0, 500.0)
        );
    }

    #[test]
    fn scrollable_frame_bounds_are_clipped_below_the_pinned_gutter() {
        let crossing = frame(1, 200.0);
        let below = frame(2, 300.0);

        assert_eq!(
            clipped_frame_screen_bounds(&crossing, 0.0, 260.0, 100.0, 700.0),
            Some(VerticalScreenBounds {
                top: 260.0,
                bottom: 300.0,
            })
        );
        assert_eq!(
            clipped_frame_screen_bounds(&crossing, 40.0, 260.0, 100.0, 700.0),
            None
        );
        assert_eq!(
            clipped_frame_screen_bounds(&below, 20.5, 260.0, 100.0, 700.0),
            Some(VerticalScreenBounds {
                top: 279.5,
                bottom: 379.5,
            })
        );
    }

    #[test]
    fn adjacent_scrolled_frames_never_overlap_after_boundary_clipping() {
        let frames = [frame(1, 200.0), frame(2, 300.0), frame(3, 400.0)];
        for scroll in [0.0, 25.5, 100.0, 240.0] {
            let visible = frames
                .iter()
                .filter_map(|frame| clipped_frame_screen_bounds(frame, scroll, 260.0, 100.0, 700.0))
                .collect::<Vec<_>>();
            assert!(visible.windows(2).all(|pair| pair[0].bottom <= pair[1].top));
        }
    }

    #[test]
    fn screen_to_content_mapping_uses_one_scroll_world_for_read_groups() {
        let scrollable = frame(1, 300.0);
        assert_eq!(
            frame_content_y_at_screen_y(&scrollable, 220.0, 100.0, 260.0, 100.0, 700.0),
            None
        );
        assert_eq!(
            frame_content_y_at_screen_y(&scrollable, 260.0, 100.0, 260.0, 100.0, 700.0),
            Some(360.0)
        );

        let mut read_group = frame(2, 120.0);
        read_group.bottom = 720.0;
        read_group.frozen_bottom = Some(142.0);
        assert_eq!(
            frame_content_y_at_screen_y(&read_group, 260.0, 300.0, 260.0, 100.0, 700.0),
            Some(560.0)
        );

        let mut split = frame(3, 100.0);
        split.bottom = 800.0;
        split.frozen_bottom = Some(260.0);
        assert_eq!(
            frame_content_y_at_screen_y(&split, 150.0, 300.0, 260.0, 100.0, 700.0),
            None
        );
        assert_eq!(
            frame_content_y_at_screen_y(&split, 260.0, 300.0, 260.0, 100.0, 700.0),
            Some(560.0)
        );

        split.bottom = 800.0;
        split.subtracks = vec![TrackSubtrack {
            label: "Reads".to_owned(),
            detail: None,
            top: 260.0,
            bottom: 900.0,
            collapsed: false,
            composite_member: None,
        }];
        assert_eq!(
            frame_content_y_at_screen_y(&split, 420.0, 300.0, 260.0, 100.0, 700.0),
            Some(720.0),
            "a read group maps through the shared lower-pane scroll"
        );
        assert_eq!(
            frame_content_y_at_screen_y(&split, 520.0, 300.0, 260.0, 100.0, 700.0),
            None,
            "the read scrollbar cannot pick through the annotation pane"
        );
    }

    #[test]
    fn deep_scroll_keeps_frame_and_scrollbar_offsets_in_lockstep() {
        let frame = frame(1, 2_400.0);
        let offset = 2_250.0;
        assert_close(frame.top - offset, 150.0);
        let rect = Rect::from_min_max(pos2(0.0, 100.0), pos2(22.0, 700.0));
        let top = scrollbar_geometry(rect, 3_000.0, 0.0);
        let middle = scrollbar_geometry(rect, 3_000.0, 1_500.0);
        let bottom = scrollbar_geometry(rect, 3_000.0, 3_000.0);
        assert_close(top.thumb.top(), top.track.top());
        assert_close(middle.thumb.center().y, middle.track.center().y);
        assert_close(bottom.thumb.bottom(), bottom.track.bottom());
        assert_close(top.track.top(), rect.top() + 5.0);
        assert_close(top.track.bottom(), rect.bottom() - 5.0);
        assert_close(
            scrollbar_offset_at_pointer(middle, 3_000.0, middle.track.top()),
            0.0,
        );
        assert_close(
            scrollbar_offset_at_pointer(middle, 3_000.0, middle.track.center().y),
            1_500.0,
        );
        assert_close(
            scrollbar_offset_at_pointer(middle, 3_000.0, middle.track.bottom()),
            3_000.0,
        );
    }

    #[test]
    fn exact_scrollbar_drag_preserves_the_pointer_position_inside_the_thumb() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(22.0, 600.0));
        let geometry = scrollbar_geometry(rect, 1_759.0, 500.0);
        let grab_offset = 9.0;
        let pointer_y = geometry.track.top()
            + (geometry.track.height() - geometry.thumb.height()) * 0.75
            + grab_offset;

        assert_close(
            scrollbar_offset_at_grab(geometry, 1_759.0, pointer_y, grab_offset),
            1_319.25,
        );
    }

    #[test]
    fn fitting_content_always_uses_a_full_thumb() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(22.0, 600.0));
        let compact = scrollbar_geometry_for_density(rect, 0.0, 0.0, MIN_VERTICAL_DENSITY);
        let first_expansion = scrollbar_geometry_for_density(
            rect,
            0.0,
            0.0,
            MIN_VERTICAL_DENSITY * (2.0_f32 * 0.012).exp(),
        );

        assert_close(compact.thumb.height(), compact.track.height());
        assert_close(first_expansion.thumb.height(), compact.thumb.height());

        let anchored =
            scrollbar_geometry_for_density_at_fraction(rect, 0.0, MIN_VERTICAL_DENSITY * 2.0, 0.5);
        assert_close(anchored.thumb.height(), compact.track.height());
        assert_close(anchored.thumb.top(), compact.track.top());
    }

    #[test]
    fn density_thumb_does_not_imply_overflow_during_a_fitting_density_gesture() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(22.0, 600.0));
        let compact = scrollbar_geometry_for_density(rect, 0.0, 0.0, MIN_VERTICAL_DENSITY);
        let grabbed_y = compact.track.center().y;
        let expanded =
            scrollbar_geometry_for_density_at_fraction(rect, 0.0, MIN_VERTICAL_DENSITY * 2.0, 0.5);

        assert_close(expanded.thumb.height(), expanded.track.height());
        assert_close(expanded.thumb.center().y, grabbed_y);
        assert_close(scrollbar_fraction_at_grab(expanded, grabbed_y, 0.5), 0.5);
    }

    #[test]
    fn density_thumb_uses_the_scrollable_height_below_pinned_tracks() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(22.0, 600.0));
        let geometry = scrollbar_geometry_for_density_in_viewport(
            rect,
            600.0,
            0.0,
            MIN_VERTICAL_DENSITY,
            300.0,
        );
        let expected_fraction = 300.0 / (300.0 + 600.0);
        assert_close(
            geometry.thumb.height(),
            geometry.track.height() * expected_fraction,
        );
    }
}
