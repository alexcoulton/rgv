use std::sync::Arc;

use egui::{
    Align2, Color32, FontId, Galley, Rect, Sense, Stroke, Ui, pos2,
    text::{LayoutJob, TextFormat, TextWrapping},
};
use rgv_core::{TrackId, TrackKind};
use rgv_layout::TextPrimitive;

use super::CanvasGeometry;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrackFrameKey {
    Reference,
    Alignment(TrackId),
    Auxiliary(TrackId),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackSubtrack {
    pub label: String,
    pub detail: Option<String>,
    pub top: f32,
    pub bottom: f32,
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
    Export(TrackFrameKey),
    Remove(TrackFrameKey),
    ReorderDropped(TrackId, f32),
    ReorderKeyboard(TrackId, i32),
    ResizeStarted(TrackId),
    ResizeDragged(TrackId, f32),
    ResizeStopped(TrackId),
    ResizeKeyboard(TrackId, f32),
}

#[derive(Default)]
pub struct TrackGutterResult {
    pub actions: Vec<TrackUiAction>,
}

const GUTTER_TEXT_GAP: f32 = 6.0;
const MAX_GUTTER_LABEL_FRACTION: f32 = 0.55;

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
) -> usize {
    let content_y = pointer_y + vertical_scroll;
    let target = frames
        .iter()
        .find_map(|frame| (content_y < (frame.top + frame.bottom) * 0.5).then_some(frame.key));
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarGeometry {
    pub track: Rect,
    pub thumb: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollbarInteraction {
    pub drag_started: bool,
    pub dragging: bool,
    pub drag_stopped: bool,
    pub drag_delta: egui::Vec2,
    pub pointer: Option<egui::Pos2>,
    pub keyboard_density_pixels: f32,
}

pub fn scrollbar_geometry(rect: Rect, maximum: f32, offset: f32) -> ScrollbarGeometry {
    let track = rect.shrink2(egui::vec2(6.0, 5.0));
    let maximum = maximum.max(0.0);
    let viewport_height = rect.height().max(1.0);
    let content_height = viewport_height + maximum;
    let thumb_height = (track.height() * viewport_height / content_height)
        .clamp(32.0_f32.min(track.height()), track.height());
    let travel = (track.height() - thumb_height).max(0.0);
    let fraction = if maximum <= f32::EPSILON {
        0.0
    } else {
        offset.clamp(0.0, maximum) / maximum
    };
    let thumb_top = track.top() + travel * fraction;
    let thumb = Rect::from_min_size(
        pos2(track.left(), thumb_top),
        egui::vec2(track.width(), thumb_height),
    );
    ScrollbarGeometry { track, thumb }
}

pub fn scrollbar_offset_at_pointer(
    geometry: ScrollbarGeometry,
    maximum: f32,
    pointer_y: f32,
) -> f32 {
    let maximum = maximum.max(0.0);
    let travel = (geometry.track.height() - geometry.thumb.height()).max(0.0);
    if maximum <= f32::EPSILON || travel <= f32::EPSILON {
        return 0.0;
    }
    let requested_top = pointer_y - geometry.thumb.height() * 0.5;
    let fraction = ((requested_top - geometry.track.top()) / travel).clamp(0.0, 1.0);
    fraction * maximum
}

pub fn paint_vertical_scrollbar(
    ui: &mut Ui,
    rect: Rect,
    maximum: f32,
    offset: &mut f32,
) -> ScrollbarInteraction {
    let maximum = maximum.max(0.0);
    *offset = offset.clamp(0.0, maximum);
    let id = egui::Id::new("track-vertical-scrollbar");
    let response = ui
        .interact(rect, id, Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::AllScroll)
        .on_hover_text(
            "Drag up/down to scroll · drag left/right to change track and read-row density",
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
            "Track overview; arrow up and down scroll, Page Up and Page Down scroll by a page, Home and End reach the stack boundaries, arrow left and right change row density",
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
    let initial_geometry = scrollbar_geometry(rect, maximum, *offset);
    if response.clicked()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        *offset = scrollbar_offset_at_pointer(initial_geometry, maximum, pointer.y);
    }

    let geometry = scrollbar_geometry(rect, maximum, *offset);
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, Color32::from_gray(250));
    painter.rect_filled(geometry.track, 0.0, Color32::WHITE);
    painter.rect_stroke(
        geometry.track,
        0.0,
        Stroke::new(0.75, Color32::from_gray(95)),
        egui::StrokeKind::Inside,
    );
    if response.hovered() || response.dragged() {
        painter.rect_filled(
            geometry.thumb,
            1.0,
            Color32::from_rgba_unmultiplied(45, 92, 175, 24),
        );
    }
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
        drag_started: response.drag_started(),
        dragging: response.dragged(),
        drag_stopped: response.drag_stopped(),
        drag_delta: response.drag_delta(),
        pointer: response.interact_pointer_pos(),
        keyboard_density_pixels,
    }
}

#[allow(clippy::too_many_lines)]
pub fn paint_track_viewport_chrome(
    ui: &mut Ui,
    geometry: CanvasGeometry,
    frames: &[TrackFrame],
    vertical_scroll: f32,
    selected: Option<TrackFrameKey>,
    selected_alignments: &[TrackId],
) -> TrackGutterResult {
    let mut result = TrackGutterResult::default();
    let painter = ui.painter();
    painter.rect_filled(geometry.gutter, 0.0, Color32::from_gray(249));
    painter.line_segment(
        [geometry.gutter.right_top(), geometry.gutter.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(175)),
    );

    for frame in frames {
        let (top, bottom) = frame_screen_bounds(frame, vertical_scroll);
        if bottom <= geometry.viewport.top() || top >= geometry.viewport.bottom() {
            continue;
        }
        let visible_top = top.max(geometry.viewport.top());
        let visible_bottom = bottom.min(geometry.viewport.bottom());
        let label_rect = Rect::from_min_max(
            pos2(geometry.gutter.left(), visible_top),
            pos2(geometry.gutter.right(), visible_bottom),
        );
        let selected_as_part_of_group = match frame.key {
            TrackFrameKey::Alignment(id) => selected_alignments.contains(&id),
            TrackFrameKey::Reference | TrackFrameKey::Auxiliary(_) => false,
        };
        if selected == Some(frame.key) || selected_as_part_of_group {
            painter.rect_filled(label_rect, 0.0, Color32::from_rgb(229, 238, 252));
        }
        let response = ui.interact(
            label_rect,
            egui::Id::new(("track-label", frame.key)),
            Sense::click_and_drag(),
        );
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
                result
                    .actions
                    .push(TrackUiAction::ToggleCollapsed(frame.key));
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
                label_rect.shrink(1.0),
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
            if !matches!(frame.key, TrackFrameKey::Reference)
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
            if !matches!(frame.key, TrackFrameKey::Reference)
                && frame.visible
                && ui
                    .button(if frame.collapsed {
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
        label_painter.text(
            pos2(label_rect.left() + 6.0, label_rect.top() + 11.0),
            Align2::LEFT_CENTER,
            if frame.collapsed { "+" } else { "−" },
            FontId::proportional(10.0),
            text_color,
        );
        paint_elided_text(
            &label_painter,
            &frame.name,
            FontId::proportional(11.0),
            text_color,
            (label_rect.width() - 25.0).max(0.0),
            pos2(label_rect.left() + 18.0, label_rect.top() + 11.0),
            false,
        );
        for subtrack in &frame.subtracks {
            let (subtrack_top, subtrack_bottom) =
                subtrack_screen_bounds(frame, subtrack, vertical_scroll);
            let subtrack_top = subtrack_top.max(label_rect.top() + 19.0);
            let subtrack_bottom = subtrack_bottom.min(label_rect.bottom());
            if subtrack_bottom <= subtrack_top {
                continue;
            }
            let y = (subtrack_top + 9.0).min(subtrack_bottom - 2.0);
            let label_font = FontId::proportional(10.0);
            let detail_font = FontId::proportional(9.0);
            let label_color = Color32::from_gray(75);
            let detail_color = Color32::from_gray(105);
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
            let available_width = (label_rect.width() - 25.0).max(0.0);
            let (label_width, detail_width) =
                gutter_row_widths(label_width, detail_width, available_width);
            paint_elided_text(
                &label_painter,
                &subtrack.label,
                label_font,
                label_color,
                label_width,
                pos2(label_rect.left() + 18.0, y),
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
        painter.line_segment(
            [
                pos2(geometry.viewport.left(), bottom),
                pos2(geometry.viewport.right(), bottom),
            ],
            Stroke::new(1.0, Color32::from_gray(205)),
        );

        let TrackFrameKey::Auxiliary(id) = frame.key else {
            continue;
        };
        if bottom < geometry.viewport.top() || bottom > geometry.viewport.bottom() {
            continue;
        }
        let handle = Rect::from_min_max(
            pos2(geometry.gutter.left(), bottom - 3.0),
            pos2(geometry.gutter.right(), bottom + 3.0),
        );
        let resize = ui.interact(handle, egui::Id::new(("track-resize", id)), Sense::drag());
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
                result
                    .actions
                    .push(TrackUiAction::ResizeKeyboard(id, delta));
            }
            painter.line_segment(
                [handle.left_center(), handle.right_center()],
                Stroke::new(2.0, Color32::BLACK),
            );
        }
        if resize.drag_started() {
            result.actions.push(TrackUiAction::ResizeStarted(id));
        }
        if resize.dragged() {
            result
                .actions
                .push(TrackUiAction::ResizeDragged(id, resize.drag_delta().y));
        }
        if resize.drag_stopped() {
            result.actions.push(TrackUiAction::ResizeStopped(id));
        }
    }
    result
}

fn frame_screen_bounds(frame: &TrackFrame, vertical_scroll: f32) -> (f32, f32) {
    frame.frozen_bottom.map_or_else(
        || (frame.top - vertical_scroll, frame.bottom - vertical_scroll),
        |frozen_bottom| {
            (
                frame.top,
                (frame.bottom - vertical_scroll).max(frozen_bottom),
            )
        },
    )
}

fn subtrack_screen_bounds(
    frame: &TrackFrame,
    subtrack: &TrackSubtrack,
    vertical_scroll: f32,
) -> (f32, f32) {
    let Some(frozen_bottom) = frame.frozen_bottom else {
        return (
            subtrack.top - vertical_scroll,
            subtrack.bottom - vertical_scroll,
        );
    };
    if subtrack.top < frozen_bottom {
        (subtrack.top, subtrack.bottom.min(frozen_bottom))
    } else {
        (
            subtrack.top,
            (subtrack.bottom - vertical_scroll).max(subtrack.top),
        )
    }
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
        assert_eq!(track_drop_index(&frames, &order, 220.0, 100.0), 1);
        assert_eq!(track_drop_index(&frames, &order, 500.0, 100.0), 2);
    }

    #[test]
    fn keyboard_reorder_stops_at_both_track_boundaries() {
        assert_eq!(keyboard_reorder_index(0, 4, -1), 0);
        assert_eq!(keyboard_reorder_index(1, 4, -1), 0);
        assert_eq!(keyboard_reorder_index(1, 4, 1), 2);
        assert_eq!(keyboard_reorder_index(3, 4, 1), 3);
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
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node
                    .label()
                    .is_some_and(|label| label.starts_with("Track overview"))
                && node.numeric_value() == Some(25.0)
        }));
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
    }

    #[test]
    fn resize_clamps_to_track_height_constraints() {
        assert_close(resized_track_height(120.0, 30.0), 150.0);
        assert_close(resized_track_height(120.0, -500.0), 40.0);
        assert_close(resized_track_height(120.0, 900.0), 600.0);
    }

    #[test]
    fn frozen_alignment_header_does_not_follow_read_scroll() {
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
                },
                TrackSubtrack {
                    label: "Reads".to_owned(),
                    detail: None,
                    top: 260.0,
                    bottom: 800.0,
                },
            ],
        };

        assert_eq!(frame_screen_bounds(&frame, 300.0), (100.0, 500.0));
        assert_eq!(
            subtrack_screen_bounds(&frame, &frame.subtracks[0], 300.0),
            (125.0, 165.0)
        );
        assert_eq!(
            subtrack_screen_bounds(&frame, &frame.subtracks[1], 300.0),
            (260.0, 500.0)
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
}
