use crate::ui::CanvasGeometry;
use crate::ui::icons::{AppIcon, paint_icon};
use crate::ui::tracks::TrackGutterResult;
use crate::ui::tracks::geometry::{
    boundary_inside_clip, frame_screen_bounds, reference_orientation_accessibility,
    subtrack_screen_bounds,
};
use crate::ui::tracks::gutter::{
    ANNOTATION_PANE_GUTTER_COLOR, READ_PANE_GUTTER_COLOR, READ_TRACK_NAME_PREFIX, TrackRenameState,
    alignment_subtrack_has_independent_visibility, begin_track_rename, editable_track_name,
    group_chevron_segments, gutter_row_widths, paint_elided_text, presentation_node_for_frame,
    track_header_label_geometry, track_rename_editor_id, track_rename_state_id,
};
use crate::ui::{
    TrackFrame, TrackFrameKey, TrackGroupChrome, TrackPane, TrackSectionChrome, TrackUiAction,
};
use egui::{Color32, FontId, Rect, Sense, Stroke, Ui, pos2};
use rgv_core::{PresentationNodeId, TrackId};

#[allow(clippy::too_many_arguments)]
pub(super) fn paint_track_section_header(
    ui: &mut Ui,
    painter: &egui::Painter,
    gutter: Rect,
    full_rect: Rect,
    channel: TrackPane,
    expanded: bool,
    track_count: usize,
    result: &mut TrackGutterResult,
) {
    if full_rect.height() <= f32::EPSILON {
        return;
    }
    let gutter_rect = Rect::from_min_max(
        pos2(gutter.left(), full_rect.top()),
        pos2(gutter.right(), full_rect.bottom()),
    );
    let (label, fill) = match channel {
        TrackPane::Annotation => ("Annotations", Color32::from_rgb(219, 230, 242)),
        TrackPane::Reads => ("Read data", Color32::from_gray(236)),
    };
    painter.rect_filled(full_rect, 0.0, fill.gamma_multiply(0.72));
    painter.rect_filled(gutter_rect, 0.0, fill);
    painter.line_segment(
        [full_rect.left_bottom(), full_rect.right_bottom()],
        Stroke::new(1.0, Color32::from_gray(165)),
    );
    let response = ui
        .interact(
            full_rect,
            egui::Id::new(("track-section-header", channel)),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let action_label = if expanded { "Minimize" } else { "Expand" };
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("{action_label} {label} section"),
        )
    });
    if response.clicked() {
        result.actions.push(TrackUiAction::ToggleSection(channel));
    }
    response.clone().on_hover_text(format!(
        "{action_label} {label} without changing child track state"
    ));
    response.context_menu(|ui| {
        if ui.button(format!("{action_label} section")).clicked() {
            result.actions.push(TrackUiAction::ToggleSection(channel));
            ui.close();
        }
        ui.separator();
        if ui.button("Collapse all rows").clicked() {
            result
                .actions
                .push(TrackUiAction::SetPaneRowsExpanded(channel, false));
            ui.close();
        }
        if ui.button("Expand all rows").clicked() {
            result
                .actions
                .push(TrackUiAction::SetPaneRowsExpanded(channel, true));
            ui.close();
        }
        if ui.button("Reset row heights").clicked() {
            result
                .actions
                .push(TrackUiAction::ResetPaneRowHeights(channel));
            ui.close();
        }
    });

    let toggle_rect = Rect::from_center_size(
        pos2(gutter_rect.left() + 14.0, gutter_rect.center().y),
        egui::vec2(16.0, 16.0),
    );
    for segment in group_chevron_segments(toggle_rect, !expanded) {
        painter.line_segment(segment, Stroke::new(1.5, Color32::from_gray(65)));
    }
    let count_label = if track_count == 1 {
        "1 track".to_owned()
    } else {
        format!("{track_count} tracks")
    };
    paint_elided_text(
        painter,
        &format!("{label} · {count_label}"),
        FontId::proportional(13.0),
        Color32::from_gray(45),
        (gutter_rect.width() - 34.0).max(0.0),
        pos2(gutter_rect.left() + 28.0, gutter_rect.center().y),
        false,
    );
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(super) fn paint_track_viewport_chrome(
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
    paint_track_viewport_chrome_with_groups(
        ui,
        geometry,
        frames,
        annotation_scroll,
        read_scroll,
        scroll_clip_top,
        annotation_clip_bottom,
        selected,
        selected_alignments,
        &[],
        &[],
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
#[cfg_attr(not(test), allow(dead_code))]
pub fn paint_track_viewport_chrome_with_groups(
    ui: &mut Ui,
    geometry: CanvasGeometry,
    frames: &[TrackFrame],
    annotation_scroll: f32,
    read_scroll: f32,
    scroll_clip_top: f32,
    annotation_clip_bottom: f32,
    selected: Option<TrackFrameKey>,
    selected_alignments: &[TrackId],
    selected_read_tracks: &[TrackId],
    track_groups: &[TrackGroupChrome],
) -> TrackGutterResult {
    paint_track_viewport_chrome_with_sections(
        ui,
        geometry,
        frames,
        annotation_scroll,
        read_scroll,
        scroll_clip_top,
        annotation_clip_bottom,
        selected,
        selected_alignments,
        selected_read_tracks,
        track_groups,
        TrackSectionChrome {
            annotation_content_top: scroll_clip_top,
            read_content_top: annotation_clip_bottom,
            ..TrackSectionChrome::default()
        },
    )
}

/// Inputs and accumulated outputs of one pass over the track gutter chrome.
///
/// [`paint_track_viewport_chrome_with_sections`] runs the steps below in a fixed order; each
/// step reads the same inputs and appends to `result`, so the order is the paint and
/// interaction order.
struct GutterChromePass<'a> {
    geometry: CanvasGeometry,
    frames: &'a [TrackFrame],
    annotation_scroll: f32,
    read_scroll: f32,
    scroll_clip_top: f32,
    annotation_clip_bottom: f32,
    selected: Option<TrackFrameKey>,
    selected_alignments: &'a [TrackId],
    selected_read_tracks: &'a [TrackId],
    track_groups: &'a [TrackGroupChrome],
    sections: TrackSectionChrome,
    painter: egui::Painter,
    /// The in-progress header rename, loaded from egui memory and stored back at the end.
    rename_state: Option<TrackRenameState>,
    result: TrackGutterResult,
}

#[allow(clippy::too_many_arguments)]
pub fn paint_track_viewport_chrome_with_sections(
    ui: &mut Ui,
    geometry: CanvasGeometry,
    frames: &[TrackFrame],
    annotation_scroll: f32,
    read_scroll: f32,
    scroll_clip_top: f32,
    annotation_clip_bottom: f32,
    selected: Option<TrackFrameKey>,
    selected_alignments: &[TrackId],
    selected_read_tracks: &[TrackId],
    track_groups: &[TrackGroupChrome],
    sections: TrackSectionChrome,
) -> TrackGutterResult {
    let rename_state_id = track_rename_state_id();
    let mut pass = GutterChromePass {
        geometry,
        frames,
        annotation_scroll,
        read_scroll,
        scroll_clip_top,
        annotation_clip_bottom,
        selected,
        selected_alignments,
        selected_read_tracks,
        track_groups,
        sections,
        painter: ui.painter().clone(),
        rename_state: ui
            .ctx()
            .data(|data| data.get_temp::<TrackRenameState>(rename_state_id)),
        result: TrackGutterResult::default(),
    };
    pass.paint_pane_backgrounds();
    pass.paint_section_headers(ui);
    for frame in frames {
        pass.paint_frame_chrome(ui, frame);
    }
    pass.paint_pane_divider(ui);
    pass.store_rename_state(ui, rename_state_id);
    pass.result
}

impl GutterChromePass<'_> {
    /// Step 1: the gutter fill for both panes and its right-hand rule.
    fn paint_pane_backgrounds(&mut self) {
        let Self {
            geometry,
            annotation_clip_bottom,
            painter,
            ..
        } = self;
        let geometry = *geometry;
        let annotation_clip_bottom = *annotation_clip_bottom;
        let painter = &*painter;
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
    }

    /// Step 2: the "Annotations" and "Reads" section headers with their expand controls.
    fn paint_section_headers(&mut self, ui: &mut Ui) {
        let Self {
            geometry,
            annotation_clip_bottom,
            sections,
            painter,
            result,
            ..
        } = self;
        let geometry = *geometry;
        let annotation_clip_bottom = *annotation_clip_bottom;
        let sections = &*sections;
        let painter = &*painter;
        let annotation_header = Rect::from_min_max(
            pos2(geometry.viewport.left(), geometry.viewport.top()),
            pos2(
                geometry.viewport.right(),
                sections
                    .annotation_content_top
                    .clamp(geometry.viewport.top(), geometry.viewport.bottom()),
            ),
        );
        let read_header = Rect::from_min_max(
            pos2(
                geometry.viewport.left(),
                annotation_clip_bottom.clamp(geometry.viewport.top(), geometry.viewport.bottom()),
            ),
            pos2(
                geometry.viewport.right(),
                sections
                    .read_content_top
                    .clamp(annotation_clip_bottom, geometry.viewport.bottom()),
            ),
        );
        paint_track_section_header(
            ui,
            painter,
            geometry.gutter,
            annotation_header,
            TrackPane::Annotation,
            sections.annotation_expanded,
            sections.annotation_track_count,
            result,
        );
        paint_track_section_header(
            ui,
            painter,
            geometry.gutter,
            read_header,
            TrackPane::Reads,
            sections.reads_expanded,
            sections.read_track_count,
            result,
        );
    }

    /// Step 3, once per frame in order: header controls, context menu, label, inline rename,
    /// subtrack rows and the resize handle of one track frame.
    #[allow(clippy::too_many_lines)]
    fn paint_frame_chrome(&mut self, ui: &mut Ui, frame: &TrackFrame) {
        let Self {
            geometry,
            annotation_scroll,
            read_scroll,
            scroll_clip_top,
            annotation_clip_bottom,
            selected,
            selected_alignments,
            selected_read_tracks,
            track_groups,
            sections,
            painter,
            rename_state,
            result,
            ..
        } = self;
        let geometry = *geometry;
        let annotation_scroll = *annotation_scroll;
        let read_scroll = *read_scroll;
        let scroll_clip_top = *scroll_clip_top;
        let annotation_clip_bottom = *annotation_clip_bottom;
        let selected = *selected;
        let selected_alignments = *selected_alignments;
        let selected_read_tracks = *selected_read_tracks;
        let track_groups = *track_groups;
        let sections = &*sections;
        let painter = &*painter;
        // The primary alignment is represented by one frame in the annotation
        // pane and another in the read pane. Keep every egui widget scoped to
        // its pane: reusing the track key alone gives both headers the same ID,
        // which makes popups and clicks fight each other between frames.
        let frame_pane = frame.channel;
        let read_pane_frame = frame_pane == TrackPane::Reads;
        let group_header = matches!(frame.key, TrackFrameKey::Group { .. });
        let only_read_data_panel =
            read_pane_frame && !frame.collapsed && sections.read_track_count == 1;
        let track_group = if let TrackFrameKey::Group {
            channel,
            id: group_id,
        } = frame.key
        {
            track_groups
                .iter()
                .find(|group| group.id == group_id && group.pane == channel)
        } else if let TrackFrameKey::Alignment(track_id) = frame.key {
            track_groups
                .iter()
                .find(|group| group.pane == frame_pane && group.contains(track_id))
        } else {
            None
        };
        let pane_selection = if read_pane_frame {
            selected_read_tracks
        } else {
            selected_alignments
        };
        let group_anchor = group_header;
        let display_name = frame.name.clone();
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
            return;
        }
        // A read panel's header stays fixed at the top of that panel while its
        // read primitives move inside the panel's independently clipped camera.
        let header_top = if read_pane_frame {
            natural_top.max(frame_clip_top).min(bottom - 22.0)
        } else {
            natural_top
        };
        let label_rect = Rect::from_min_max(
            pos2(geometry.gutter.left(), visible_top),
            pos2(geometry.gutter.right(), visible_bottom),
        );
        if track_group.is_some() {
            let rail_x = label_rect.left() + 21.0;
            painter.line_segment(
                [
                    pos2(rail_x, label_rect.top()),
                    pos2(rail_x, label_rect.bottom()),
                ],
                Stroke::new(1.0, Color32::from_rgb(126, 154, 189)),
            );
            if group_anchor {
                painter.line_segment(
                    [
                        pos2(rail_x, label_rect.top()),
                        pos2(label_rect.right(), label_rect.top()),
                    ],
                    Stroke::new(1.0, Color32::from_rgb(126, 154, 189)),
                );
            }
        }
        let selected_as_part_of_group = match frame.key {
            TrackFrameKey::Alignment(id) | TrackFrameKey::Auxiliary(id) if !read_pane_frame => {
                selected_alignments.contains(&id)
            }
            TrackFrameKey::Reference | TrackFrameKey::Group { .. } => false,
            TrackFrameKey::Alignment(_) => false,
            TrackFrameKey::Auxiliary(_) => false,
        };
        let selection_highlighted = if read_pane_frame {
            matches!(frame.key, TrackFrameKey::Alignment(id) if selected_read_tracks.contains(&id))
        } else {
            match frame.key {
                TrackFrameKey::Alignment(_) | TrackFrameKey::Auxiliary(_) => {
                    selected_as_part_of_group
                }
                TrackFrameKey::Group { .. } | TrackFrameKey::Reference => {
                    selected == Some(frame.key)
                }
            }
        };
        if selection_highlighted {
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
        let renaming_this_header = rename_state
            .as_ref()
            .is_some_and(|state| state.key == frame.key && state.pane == frame_pane);
        let eye_rect = Rect::from_center_size(
            pos2(raw_header_rect.left() + 11.0, raw_header_rect.center().y),
            egui::vec2(16.0, 16.0),
        );
        let settings_rect = Rect::from_center_size(
            pos2(raw_header_rect.right() - 11.0, raw_header_rect.center().y),
            egui::vec2(18.0, 18.0),
        );
        let group_toggle_rect = Rect::from_center_size(
            pos2(raw_header_rect.left() + 30.0, raw_header_rect.center().y),
            egui::vec2(16.0, 16.0),
        );
        let response = ui.interact(
            Rect::from_min_max(
                pos2(
                    raw_header_rect.left() + if group_header { 40.0 } else { 22.0 },
                    raw_header_rect.top(),
                ),
                pos2(settings_rect.left(), label_rect.bottom()),
            )
            .intersect(label_rect),
            egui::Id::new(("track-label", frame.key, read_pane_frame)),
            if renaming_this_header {
                Sense::hover()
            } else {
                Sense::click_and_drag()
            },
        );
        let eye = ui.interact(
            eye_rect.intersect(header_rect),
            egui::Id::new(("track-collapse", frame.key, read_pane_frame)),
            Sense::click(),
        );
        let group_toggle = group_header.then(|| {
            ui.interact(
                group_toggle_rect.intersect(header_rect),
                egui::Id::new(("track-group-toggle", frame.key, read_pane_frame)),
                Sense::click(),
            )
        });
        let settings = ui.interact(
            settings_rect.intersect(header_rect),
            egui::Id::new(("track-settings", frame.key, read_pane_frame)),
            Sense::click(),
        );
        let is_composite = frame
            .subtracks
            .iter()
            .any(|subtrack| subtrack.composite_member.is_some());
        let content_hidden = if group_header {
            track_group.is_some_and(|group| !group.content_visible)
        } else if is_composite {
            !frame.visible
        } else {
            !frame.visible || frame.collapsed
        };
        let frame_eye_action = if let Some(group) = track_group.filter(|_| group_header) {
            TrackUiAction::ToggleTrackGroupVisibility(frame_pane, group.id)
        } else if is_composite {
            TrackUiAction::ToggleCompositeVisibility(frame.key)
        } else {
            let target = match frame.key {
                TrackFrameKey::Reference => PresentationNodeId::Reference,
                TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
                    PresentationNodeId::Track {
                        channel: frame_pane,
                        track_id,
                    }
                }
                TrackFrameKey::Group { .. } => unreachable!("group handled above"),
            };
            TrackUiAction::SetNodeVisibility(target, content_hidden)
        };
        eye.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                true,
                !content_hidden,
                if read_pane_frame {
                    format!(
                        "{} read content {}",
                        display_name,
                        if content_hidden { "hidden" } else { "visible" }
                    )
                } else if is_composite {
                    format!(
                        "{} composite {}",
                        display_name,
                        if content_hidden { "hidden" } else { "visible" }
                    )
                } else {
                    format!(
                        "{} track content {}",
                        display_name,
                        if content_hidden { "hidden" } else { "visible" }
                    )
                },
            )
        });
        if eye.clicked() {
            result.actions.push(frame_eye_action.clone());
        }
        if let (Some(toggle), Some(group)) = (&group_toggle, track_group.filter(|_| group_header)) {
            toggle.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    if group.collapsed {
                        format!("Expand group {}", group.id)
                    } else {
                        format!("Minimize group {}", group.id)
                    },
                )
            });
            if toggle.clicked() {
                result
                    .actions
                    .push(TrackUiAction::ToggleTrackGroup(frame_pane, group.id));
            }
            toggle.clone().on_hover_text(if group.collapsed {
                format!("Expand group {}", group.id)
            } else {
                format!("Minimize group {}", group.id)
            });
        }
        eye.clone()
            .on_hover_text(if let Some(group) = track_group.filter(|_| group_header) {
                if group.content_visible {
                    format!("Hide all content in group {}", group.id)
                } else {
                    format!("Show all content in group {}", group.id)
                }
            } else if !frame.visible {
                format!("Show {display_name}")
            } else if read_pane_frame {
                if content_hidden {
                    "Show reads".to_owned()
                } else {
                    "Hide reads".to_owned()
                }
            } else if is_composite {
                if content_hidden {
                    format!("Show all layers in {display_name}")
                } else {
                    format!("Hide all layers in {display_name}")
                }
            } else if content_hidden {
                format!("Show all content in {display_name}")
            } else {
                format!("Hide all content in {display_name}")
            });
        if !group_header {
            settings.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    format!("Open settings for {display_name}"),
                )
            });
        }
        if settings.clicked() && !group_header {
            result
                .actions
                .push(TrackUiAction::Select(frame.key, frame_pane));
            result.actions.push(TrackUiAction::OpenSettings(frame.key));
        }
        if !group_header {
            settings.clone().on_hover_text("Track settings");
        }
        let (label_position, label_width) =
            track_header_label_geometry(label_rect, header_top, group_header);
        let rename_hit_rect = Rect::from_min_max(
            pos2(label_position.x, raw_header_rect.top()),
            pos2(
                (label_position.x + label_width).min(settings_rect.left()),
                raw_header_rect.bottom(),
            ),
        )
        .intersect(header_rect);
        let rename_requested_by_double_click = editable_track_name(frame).is_some()
            && response.double_clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|pointer| rename_hit_rect.contains(pointer));
        if rename_requested_by_double_click {
            *rename_state = begin_track_rename(frame);
            ui.ctx().request_repaint();
        }
        if response.clicked() {
            result
                .actions
                .push(TrackUiAction::Select(frame.key, frame_pane));
        }
        if response.has_focus() && !renaming_this_header {
            let mut open_settings = false;
            let mut begin_rename = false;
            let mut toggle_visibility = false;
            let mut toggle_collapsed = false;
            let mut reorder = 0;
            let mut remove = false;
            ui.input_mut(|input| {
                open_settings = input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                begin_rename = input.consume_key(egui::Modifiers::NONE, egui::Key::F2);
                toggle_visibility = input.consume_key(egui::Modifiers::NONE, egui::Key::V);
                toggle_collapsed = input.consume_key(egui::Modifiers::NONE, egui::Key::C);
                if input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp) {
                    reorder = -1;
                } else if input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowDown) {
                    reorder = 1;
                }
                remove = input.consume_key(egui::Modifiers::SHIFT, egui::Key::Delete);
            });
            if open_settings && !group_header {
                result
                    .actions
                    .push(TrackUiAction::Select(frame.key, frame_pane));
                result.actions.push(TrackUiAction::OpenSettings(frame.key));
            }
            if begin_rename && editable_track_name(frame).is_some() {
                *rename_state = begin_track_rename(frame);
                ui.ctx().request_repaint();
            }
            if toggle_visibility && !group_header {
                result.actions.push(frame_eye_action.clone());
            }
            if toggle_collapsed && frame.visible && !matches!(frame.key, TrackFrameKey::Reference) {
                if let Some(group) = track_group.filter(|_| group_header) {
                    result
                        .actions
                        .push(TrackUiAction::ToggleTrackGroup(frame_pane, group.id));
                } else {
                    result.actions.push(frame_eye_action.clone());
                }
            }
            if let Some(target) = presentation_node_for_frame(frame) {
                if reorder != 0 {
                    result
                        .actions
                        .push(TrackUiAction::ReorderKeyboard(target, reorder));
                }
                if remove && !group_header {
                    result.actions.push(TrackUiAction::Remove(frame.key));
                }
            }
        }
        if !renaming_this_header
            && response.drag_stopped()
            && let Some(target) = presentation_node_for_frame(frame)
            && let Some(pointer) = response.interact_pointer_pos()
        {
            result
                .actions
                .push(TrackUiAction::ReorderDropped(target, pointer.y));
        }
        response.widget_info(|| {
            let interaction_help = if group_header {
                "use the eye to toggle group content and the chevron to minimize or expand"
                    .to_owned()
            } else if editable_track_name(frame).is_some() {
                "double-click or F2 renames, Enter opens settings, V toggles visibility, C toggles collapse, Alt plus arrow up or down reorders, Shift plus Delete removes".to_owned()
            } else {
                "Enter opens settings, V toggles visibility, C toggles collapse, Alt plus arrow up or down reorders, Shift plus Delete removes".to_owned()
            };
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                selection_highlighted,
                format!(
                    "{} {:?} track; {interaction_help}",
                    display_name, frame.kind,
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
        let mut tooltip = if group_header {
            format!("{display_name}\nTrack group")
        } else {
            format!("{display_name}\n{:?} track", frame.kind)
        };
        if let Some(group) = track_group {
            tooltip.push_str(&format!(
                "\nTrack group {} ({} members)",
                group.id,
                group.members.len()
            ));
        }
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
        // Track menus are direct-manipulation controls. Egui fades new areas
        // using the global animation duration, which makes a right-click feel
        // delayed. Override it only while this popup is painted so other UI
        // motion continues to respect the accessibility preference.
        let theme = ui.ctx().theme();
        let animation_time = ui.ctx().style_of(theme).animation_time;
        ui.ctx()
            .style_mut_of(theme, |style| style.animation_time = 0.0);
        response.context_menu(|ui| {
            if editable_track_name(frame).is_some() && ui.button("Rename…").clicked() {
                *rename_state = begin_track_rename(frame);
                ui.ctx().request_repaint();
                ui.close();
            }
            if !group_header && ui.button("Track settings…").clicked() {
                result.actions.push(TrackUiAction::OpenSettings(frame.key));
                ui.close();
            }
            if read_pane_frame
                && let TrackFrameKey::Alignment(track_id) = frame.key
                && ui
                    .button(if content_hidden {
                        "Show reads"
                    } else {
                        "Hide reads"
                    })
                    .clicked()
            {
                result.actions.push(TrackUiAction::SetNodeVisibility(
                    PresentationNodeId::Track {
                        channel: TrackPane::Reads,
                        track_id,
                    },
                    content_hidden,
                ));
                ui.close();
            }
            if !read_pane_frame
                && !matches!(
                    frame.key,
                    TrackFrameKey::Reference | TrackFrameKey::Group { .. }
                )
                && ui
                    .button(if content_hidden {
                        "Show track"
                    } else {
                        "Hide track"
                    })
                    .clicked()
            {
                let target = match frame.key {
                    TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
                        PresentationNodeId::Track {
                            channel: frame_pane,
                            track_id,
                        }
                    }
                    TrackFrameKey::Reference | TrackFrameKey::Group { .. } => unreachable!(),
                };
                result
                    .actions
                    .push(TrackUiAction::SetNodeVisibility(target, content_hidden));
                ui.close();
            }
            if let TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) =
                frame.key
                && pane_selection.len() > 1
                && pane_selection.contains(&track_id)
                && ui
                    .button(format!("Group {} selected tracks", pane_selection.len()))
                    .clicked()
            {
                result
                    .actions
                    .push(TrackUiAction::GroupSelectedTracks(frame_pane));
                ui.close();
            }
            if let Some(group) = track_group
                && group_header
                && ui
                    .button(if group.collapsed {
                        "Expand group"
                    } else {
                        "Minimize group"
                    })
                    .clicked()
            {
                result
                    .actions
                    .push(TrackUiAction::ToggleTrackGroup(frame_pane, group.id));
                ui.close();
            }
            if let Some(group) = track_group
                && ui.button("Ungroup tracks").clicked()
            {
                result
                    .actions
                    .push(TrackUiAction::UngroupTracks(frame_pane, group.id));
                ui.close();
            }
            if !read_pane_frame
                && (frame.visible || is_composite)
                && !group_header
                && ui
                    .button(if is_composite {
                        if content_hidden {
                            "Show overlay content"
                        } else {
                            "Hide overlay content"
                        }
                    } else if frame.collapsed {
                        "Expand track"
                    } else {
                        "Collapse track"
                    })
                    .clicked()
            {
                result.actions.push(if is_composite {
                    TrackUiAction::ToggleCompositeVisibility(frame.key)
                } else {
                    TrackUiAction::ToggleCollapsed(frame.key)
                });
                ui.close();
            }
            if !group_header && ui.button("Export current view…").clicked() {
                result.actions.push(TrackUiAction::Export(frame.key));
                ui.close();
            }
            if !matches!(
                frame.key,
                TrackFrameKey::Reference | TrackFrameKey::Group { .. }
            ) && !matches!(frame.key, TrackFrameKey::Alignment(id) if id.get() == 0)
                && ui.button("Remove track").clicked()
            {
                result.actions.push(TrackUiAction::Remove(frame.key));
                ui.close();
            }
        });
        ui.ctx()
            .style_mut_of(theme, |style| style.animation_time = animation_time);
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
        if content_hidden {
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
        if let (Some(toggle), Some(group)) = (&group_toggle, track_group.filter(|_| group_header)) {
            let chevron_color = if toggle.hovered() {
                Color32::from_rgb(45, 92, 175)
            } else {
                Color32::from_gray(90)
            };
            for segment in group_chevron_segments(group_toggle_rect, group.collapsed) {
                label_painter.line_segment(segment, Stroke::new(1.5, chevron_color));
            }
        }
        if !group_header {
            if settings.hovered() {
                label_painter.rect_filled(
                    settings_rect,
                    3.0,
                    Color32::from_rgba_unmultiplied(45, 92, 175, 24),
                );
            }
            paint_icon(
                &label_painter,
                settings_rect.shrink(3.0),
                AppIcon::Settings,
                if settings.hovered() {
                    Color32::from_rgb(45, 92, 175)
                } else {
                    Color32::from_gray(95)
                },
            );
        }
        let mut rename_resolution = None;
        if let Some(state) = rename_state
            .as_mut()
            .filter(|state| state.key == frame.key && state.pane == frame_pane)
        {
            let font_id = FontId::proportional(11.0);
            let prefix = (frame_pane == TrackPane::Reads
                && matches!(frame.key, TrackFrameKey::Alignment(_)))
            .then_some(READ_TRACK_NAME_PREFIX);
            let prefix_width = prefix.map_or(0.0, |prefix| {
                let width = label_painter
                    .layout_no_wrap(prefix.to_owned(), font_id.clone(), text_color)
                    .size()
                    .x;
                paint_elided_text(
                    &label_painter,
                    prefix,
                    font_id.clone(),
                    text_color,
                    width.min(label_width),
                    label_position,
                    false,
                );
                width
            });
            let editor_left = label_position.x + prefix_width;
            let editor_right = (label_position.x + label_width).min(settings_rect.left());
            if editor_right - editor_left >= 8.0 {
                let editor_id = track_rename_editor_id(frame.key, frame_pane);
                let editor_rect = Rect::from_min_max(
                    pos2(editor_left, raw_header_rect.top() + 2.0),
                    pos2(editor_right, raw_header_rect.bottom() - 2.0),
                )
                .intersect(header_rect);
                let editor = ui.put(
                    editor_rect,
                    egui::TextEdit::singleline(&mut state.draft)
                        .id(editor_id)
                        .font(font_id)
                        .desired_width(editor_rect.width())
                        .cursor_at_end(true),
                );
                if state.request_focus {
                    editor.request_focus();
                    state.request_focus = false;
                }
                // Egui clears widget focus for Escape before TextEdit renders,
                // so cancellation must be keyed to the active rename state,
                // not to `editor.has_focus()` after event processing.
                let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
                let commit =
                    editor.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if escape {
                    rename_resolution = Some(None);
                } else if commit || editor.lost_focus() {
                    let name = state.draft.trim();
                    rename_resolution = Some((!name.is_empty()).then(|| name.to_owned()));
                }
            } else {
                paint_elided_text(
                    &label_painter,
                    &display_name,
                    font_id,
                    text_color,
                    label_width,
                    label_position,
                    false,
                );
            }
        } else {
            paint_elided_text(
                &label_painter,
                &display_name,
                FontId::proportional(11.0),
                text_color,
                label_width,
                label_position,
                false,
            );
        }
        if let Some(committed_name) = rename_resolution {
            *rename_state = None;
            if let Some(name) = committed_name {
                result
                    .actions
                    .push(TrackUiAction::RenameTrack(frame.key, name));
            }
        }
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
                TrackFrameKey::Alignment(_) => subtrack.alignment_subtrack,
                TrackFrameKey::Reference
                | TrackFrameKey::Group { .. }
                | TrackFrameKey::Auxiliary(_) => None,
            };
            if let (TrackFrameKey::Alignment(track_id), Some(alignment_subtrack)) =
                (frame.key, alignment_subtrack)
                && alignment_subtrack_has_independent_visibility(frame, alignment_subtrack)
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
        if !only_read_data_panel && boundary_inside_clip(bottom, frame_clip_top, frame_clip_bottom)
        {
            painter.line_segment(
                [
                    pos2(geometry.viewport.left(), bottom),
                    pos2(geometry.viewport.right(), bottom),
                ],
                Stroke::new(1.0, Color32::from_gray(205)),
            );
        }

        if frame.collapsed || only_read_data_panel {
            return;
        }
        if !boundary_inside_clip(bottom, frame_clip_top, frame_clip_bottom) {
            return;
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
                        return;
                    };
                    result.actions.push(TrackUiAction::ResizeReadTrackKeyboard(
                        id,
                        frame.bottom - frame.top,
                        delta,
                    ));
                } else {
                    match frame.key {
                        TrackFrameKey::Reference
                        | TrackFrameKey::Alignment(_)
                        | TrackFrameKey::Auxiliary(_) => {
                            result.actions.push(TrackUiAction::ResizeAnnotationKeyboard(
                                frame.key,
                                frame.bottom - frame.top,
                                delta,
                            ));
                        }
                        TrackFrameKey::Group { .. } => {}
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
                    return;
                };
                result.actions.push(TrackUiAction::ResizeReadTrackStarted(
                    id,
                    frame.bottom - frame.top,
                ));
            } else {
                match frame.key {
                    TrackFrameKey::Reference
                    | TrackFrameKey::Alignment(_)
                    | TrackFrameKey::Auxiliary(_) => {
                        result.actions.push(TrackUiAction::ResizeAnnotationStarted(
                            frame.key,
                            frame.bottom - frame.top,
                        ));
                    }
                    TrackFrameKey::Group { .. } => {}
                }
            }
        }
        if resize.dragged() {
            if read_pane_frame {
                let TrackFrameKey::Alignment(id) = frame.key else {
                    return;
                };
                let delta = resize.drag_delta().y;
                if delta != 0.0 {
                    result
                        .actions
                        .push(TrackUiAction::ResizeReadTrackDragged(id, delta));
                }
            } else {
                match frame.key {
                    TrackFrameKey::Reference
                    | TrackFrameKey::Alignment(_)
                    | TrackFrameKey::Auxiliary(_) => result.actions.push(
                        TrackUiAction::ResizeAnnotationDragged(frame.key, resize.drag_delta().y),
                    ),
                    TrackFrameKey::Group { .. } => {}
                }
            }
        }
        if resize.drag_stopped() {
            if read_pane_frame {
                let TrackFrameKey::Alignment(id) = frame.key else {
                    return;
                };
                result
                    .actions
                    .push(TrackUiAction::ResizeReadTrackStopped(id));
            } else {
                match frame.key {
                    TrackFrameKey::Reference
                    | TrackFrameKey::Alignment(_)
                    | TrackFrameKey::Auxiliary(_) => result
                        .actions
                        .push(TrackUiAction::ResizeAnnotationStopped(frame.key)),
                    TrackFrameKey::Group { .. } => {}
                }
            }
        }
    }

    /// Step 4: the draggable divider between the annotation and read panes.
    fn paint_pane_divider(&mut self, ui: &mut Ui) {
        let Self {
            geometry,
            annotation_clip_bottom,
            sections,
            painter,
            result,
            ..
        } = self;
        let geometry = *geometry;
        let annotation_clip_bottom = *annotation_clip_bottom;
        let sections = &*sections;
        let painter = &*painter;
        if sections.annotation_expanded
            && sections.reads_expanded
            && annotation_clip_bottom > geometry.viewport.top()
            && annotation_clip_bottom < geometry.viewport.bottom()
        {
            let divider_handle = Rect::from_min_max(
                pos2(geometry.gutter.left(), annotation_clip_bottom - 6.0),
                pos2(geometry.scrollbar.left(), annotation_clip_bottom + 6.0),
            );
            result.pane_divider_rect = Some(divider_handle);
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
                if divider.hovered() || divider.dragged() {
                    Stroke::new(3.0, Color32::from_rgb(45, 92, 175))
                } else {
                    Stroke::new(1.5, Color32::from_gray(165))
                },
            );
        }
    }

    /// Step 5: drop a rename whose frame disappeared, then persist the rename state.
    fn store_rename_state(&mut self, ui: &mut Ui, rename_state_id: egui::Id) {
        let frames = self.frames;
        let mut rename_state = self.rename_state.take();
        if rename_state.as_ref().is_some_and(|state| {
            !frames
                .iter()
                .any(|frame| frame.key == state.key && frame.channel == state.pane)
        }) {
            rename_state = None;
        }
        ui.ctx().data_mut(|data| {
            if let Some(state) = rename_state {
                data.insert_temp(rename_state_id, state);
            } else {
                data.remove::<TrackRenameState>(rename_state_id);
            }
        });
    }
}
