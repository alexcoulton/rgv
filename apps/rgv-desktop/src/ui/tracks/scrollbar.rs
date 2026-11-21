use crate::ui::TrackFrame;
use crate::{MAX_VERTICAL_DENSITY, MIN_VERTICAL_DENSITY};
use egui::{Color32, Rect, Sense, Stroke, Ui, pos2};
use rgv_core::{PanelId, TrackId, TrackKind};
use rgv_layout::DisplayList;

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
#[cfg_attr(not(test), allow(dead_code))]
pub fn paint_annotation_scrollbar(
    ui: &mut Ui,
    rect: Rect,
    maximum: f32,
    offset: &mut f32,
    overview: Option<&ScrollbarOverview>,
) -> ScrollbarInteraction {
    paint_annotation_scrollbar_with_id(
        ui,
        rect,
        egui::Id::new("annotation-vertical-scrollbar"),
        egui::Id::new("annotation-scrollbar-drag-state"),
        maximum,
        offset,
        overview,
        "Annotation tracks · drag up/down to scroll",
        "Annotation track scrollbar; arrow up and down scroll, Page Up and Page Down scroll by a page, Home and End reach the stack boundaries",
    )
}

pub fn paint_workspace_annotation_scrollbar(
    ui: &mut Ui,
    rect: Rect,
    panel_id: PanelId,
    maximum: f32,
    offset: &mut f32,
    overview: Option<&ScrollbarOverview>,
) -> ScrollbarInteraction {
    paint_annotation_scrollbar_with_id(
        ui,
        rect,
        egui::Id::new(("annotation-vertical-scrollbar", panel_id.get())),
        egui::Id::new(("annotation-scrollbar-drag-state", panel_id.get())),
        maximum,
        offset,
        overview,
        "Annotation tracks · drag up/down to scroll",
        "Annotation track scrollbar; arrow up and down scroll, Page Up and Page Down scroll by a page, Home and End reach the stack boundaries",
    )
}

pub fn paint_read_stack_scrollbar(
    ui: &mut Ui,
    rect: Rect,
    panel_id: PanelId,
    maximum: f32,
    offset: &mut f32,
) -> ScrollbarInteraction {
    paint_annotation_scrollbar_with_id(
        ui,
        rect,
        egui::Id::new(("read-stack-scrollbar", panel_id.get())),
        egui::Id::new(("read-stack-scrollbar-drag-state", panel_id.get())),
        maximum,
        offset,
        None,
        "Read-panel stack · drag up/down to scroll panels",
        "Read-panel stack scrollbar; arrow up and down scroll, Page Up and Page Down scroll by a page, Home and End reach the panel boundaries",
    )
}

pub(super) fn paint_annotation_scrollbar_with_id(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    drag_state_id: egui::Id,
    maximum: f32,
    offset: &mut f32,
    overview: Option<&ScrollbarOverview>,
    hover_text: &str,
    accessibility_label: &str,
) -> ScrollbarInteraction {
    let maximum = maximum.max(0.0);
    *offset = offset.clamp(0.0, maximum);
    let response = ui
        .interact(rect, id, Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical)
        .on_hover_text(hover_text);
    let scroll_percent = if maximum <= f32::EPSILON {
        0.0
    } else {
        f64::from(*offset / maximum * 100.0)
    };
    response.widget_info(|| egui::WidgetInfo::slider(true, scroll_percent, accessibility_label));
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
    painter.rect_filled(rect, 0.0, crate::theme::adapt(Color32::from_gray(250)));
    painter.rect_filled(geometry.track, 0.0, crate::theme::adapt(Color32::WHITE));
    if let Some(overview) = overview.filter(|overview| !overview.bins.is_empty()) {
        for (index, bin) in overview.bins.iter().enumerate() {
            painter.rect_filled(
                scrollbar_overview_bin_rect(geometry.track, index, overview.bins.len()),
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
                Stroke::new(0.75, crate::theme::adapt(Color32::from_gray(72))),
            );
        }
    }
    painter.rect_stroke(
        geometry.track,
        0.0,
        Stroke::new(0.75, crate::theme::adapt(Color32::from_gray(95))),
        egui::StrokeKind::Inside,
    );
    painter.rect_filled(
        geometry.thumb,
        1.0,
        crate::theme::adapt(Color32::from_rgba_unmultiplied(75, 91, 112, 42)),
    );
    painter.rect_stroke(
        geometry.thumb,
        1.0,
        Stroke::new(1.5, crate::theme::adapt(Color32::from_rgb(75, 91, 112))),
        egui::StrokeKind::Outside,
    );
    if response.has_focus() {
        painter.rect_stroke(
            rect.shrink(1.0),
            1.0,
            Stroke::new(2.0, crate::theme::adapt(Color32::BLACK)),
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

pub(super) fn scrollbar_geometry_with_visible_fraction(
    rect: Rect,
    maximum: f32,
    camera_viewport_height: f32,
    scale_visible_fraction: f32,
    scroll_fraction: f32,
) -> ScrollbarGeometry {
    let track = rect.shrink2(egui::vec2((rect.width() * 0.25).min(6.0), 5.0));
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

pub(super) fn scrollbar_overview_bin_rect(track: Rect, index: usize, bin_count: usize) -> Rect {
    debug_assert!(bin_count > 0);
    debug_assert!(index < bin_count);
    let bin_height = track.height() / bin_count as f32;
    let top = track.top() + index as f32 * bin_height;
    let bottom = if index + 1 == bin_count {
        track.bottom()
    } else {
        top + bin_height + 0.5
    };
    Rect::from_min_max(pos2(track.left(), top), pos2(track.right(), bottom))
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
#[cfg_attr(not(test), allow(dead_code))]
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
    paint_vertical_scrollbar_with_id(
        ui,
        rect,
        egui::Id::new("track-vertical-scrollbar"),
        maximum,
        offset,
        density,
        camera_viewport_height,
        preview_scroll_fraction,
        overview,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn paint_read_panel_scrollbar(
    ui: &mut Ui,
    rect: Rect,
    panel_id: PanelId,
    track_id: TrackId,
    maximum: f32,
    offset: &mut f32,
    density: f32,
    camera_viewport_height: f32,
    preview_scroll_fraction: Option<f32>,
    overview: Option<&ScrollbarOverview>,
    provisional: bool,
) -> ScrollbarInteraction {
    paint_vertical_scrollbar_with_id(
        ui,
        rect,
        egui::Id::new((
            "read-panel-vertical-scrollbar",
            panel_id.get(),
            track_id.get(),
        )),
        maximum,
        offset,
        density,
        camera_viewport_height,
        preview_scroll_fraction,
        overview,
        provisional,
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn paint_vertical_scrollbar_with_id(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    maximum: f32,
    offset: &mut f32,
    density: f32,
    camera_viewport_height: f32,
    preview_scroll_fraction: Option<f32>,
    overview: Option<&ScrollbarOverview>,
    provisional: bool,
) -> ScrollbarInteraction {
    let maximum = maximum.max(0.0);
    *offset = offset.clamp(0.0, maximum);
    let response = ui
        .interact(
            rect,
            id,
            if provisional {
                Sense::hover()
            } else {
                Sense::click_and_drag()
            },
        )
        .on_hover_cursor(if provisional {
            egui::CursorIcon::Wait
        } else {
            egui::CursorIcon::AllScroll
        })
        .on_hover_text(if provisional {
            "Read stack extent is updating · previous position shown"
        } else {
            "Read stack · drag up/down to scroll · drag left/right to change read-row density"
        });
    let scroll_percent = if maximum <= f32::EPSILON {
        0.0
    } else {
        f64::from(*offset / maximum * 100.0)
    };
    response.widget_info(|| {
        egui::WidgetInfo::slider(
            !provisional,
            scroll_percent,
            "Read stack overview; arrow up and down scroll, Page Up and Page Down scroll by a page, Home and End reach the read boundaries, arrow left and right change read-row density",
        )
    });
    let mut keyboard_density_pixels = 0.0;
    if response.has_focus() && !provisional {
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
    let pointer_down = !provisional && (response.is_pointer_button_down_on() || response.dragged());
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
    painter.rect_filled(rect, 0.0, crate::theme::adapt(Color32::from_gray(250)));
    painter.rect_filled(geometry.track, 0.0, crate::theme::adapt(Color32::WHITE));
    if let Some(overview) = overview.filter(|overview| !overview.bins.is_empty()) {
        for (index, bin) in overview.bins.iter().enumerate() {
            painter.rect_filled(
                scrollbar_overview_bin_rect(geometry.track, index, overview.bins.len()),
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
                Stroke::new(0.75, crate::theme::adapt(Color32::from_gray(72))),
            );
        }
    }
    painter.rect_stroke(
        geometry.track,
        0.0,
        Stroke::new(0.75, crate::theme::adapt(Color32::from_gray(95))),
        egui::StrokeKind::Inside,
    );
    if provisional {
        painter.rect_filled(
            geometry.track,
            0.0,
            crate::theme::adapt(Color32::from_rgba_unmultiplied(245, 247, 250, 150)),
        );
    }
    painter.rect_filled(
        geometry.thumb,
        1.0,
        if provisional {
            crate::theme::adapt(Color32::from_rgba_unmultiplied(110, 116, 124, 42))
        } else {
            crate::theme::adapt(Color32::from_rgba_unmultiplied(
                45,
                92,
                175,
                if response.hovered() || response.dragged() {
                    52
                } else {
                    30
                },
            ))
        },
    );
    painter.rect_stroke(
        geometry.thumb,
        1.0,
        Stroke::new(
            if provisional { 1.0 } else { 2.0 },
            if provisional {
                crate::theme::adapt(Color32::from_gray(110))
            } else {
                crate::theme::adapt(Color32::from_rgb(45, 92, 175))
            },
        ),
        egui::StrokeKind::Outside,
    );
    if response.has_focus() && !provisional {
        painter.rect_stroke(
            rect.shrink(1.0),
            1.0,
            Stroke::new(2.0, crate::theme::adapt(Color32::BLACK)),
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

pub(super) fn scrollbar_overview_color(kind: Option<TrackKind>, density: f32) -> Color32 {
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
    crate::theme::adapt(Color32::from_rgb(blend(red), blend(green), blend(blue)))
}
