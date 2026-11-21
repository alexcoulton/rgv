use crate::ui::{AlignmentSubtrack, TrackFrame, TrackFrameKey, TrackPane};
use egui::{
    Color32, FontId, Galley, Rect, pos2,
    text::{LayoutJob, TextFormat, TextWrapping},
};
use rgv_core::{PresentationNodeId, TrackId};
use std::sync::Arc;

pub(super) fn presentation_node_for_frame(frame: &TrackFrame) -> Option<PresentationNodeId> {
    match frame.key {
        TrackFrameKey::Reference => None,
        TrackFrameKey::Group { channel, id } => Some(PresentationNodeId::Group {
            channel,
            group_id: id,
        }),
        TrackFrameKey::Alignment(id) if id.get() == 0 => None,
        TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
            Some(PresentationNodeId::Track {
                channel: frame.channel,
                track_id,
            })
        }
    }
}

pub(super) fn group_chevron_segments(rect: Rect, collapsed: bool) -> [[egui::Pos2; 2]; 2] {
    let center = rect.center();
    if collapsed {
        let left_top = pos2(center.x - 2.5, center.y - 4.0);
        let tip = pos2(center.x + 2.5, center.y);
        let left_bottom = pos2(center.x - 2.5, center.y + 4.0);
        [[left_top, tip], [tip, left_bottom]]
    } else {
        let left_top = pos2(center.x - 4.0, center.y - 2.5);
        let tip = pos2(center.x, center.y + 2.5);
        let right_top = pos2(center.x + 4.0, center.y - 2.5);
        [[left_top, tip], [tip, right_top]]
    }
}

pub(super) fn track_header_label_geometry(
    label_rect: Rect,
    header_top: f32,
    group_header: bool,
) -> (egui::Pos2, f32) {
    let left_offset = if group_header { 40.0 } else { 22.0 };
    let right_padding = if group_header { 4.0 } else { 31.0 };
    (
        pos2(label_rect.left() + left_offset, header_top + 11.0),
        (label_rect.width() - left_offset - right_padding).max(0.0),
    )
}

pub(super) const READ_TRACK_NAME_PREFIX: &str = "Reads · ";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TrackRenameState {
    pub(super) key: TrackFrameKey,
    pub(super) pane: TrackPane,
    pub(super) draft: String,
    pub(super) request_focus: bool,
}

pub(super) fn track_rename_state_id() -> egui::Id {
    egui::Id::new("track-inline-rename-state")
}

pub(super) fn track_rename_editor_id(key: TrackFrameKey, pane: TrackPane) -> egui::Id {
    egui::Id::new(("track-inline-rename-editor", key, pane))
}

pub(super) fn editable_track_name(frame: &TrackFrame) -> Option<&str> {
    match frame.key {
        TrackFrameKey::Alignment(_) | TrackFrameKey::Auxiliary(_) => {
            Some(if frame.channel == TrackPane::Reads {
                frame
                    .name
                    .strip_prefix(READ_TRACK_NAME_PREFIX)
                    .unwrap_or(&frame.name)
            } else {
                &frame.name
            })
        }
        TrackFrameKey::Reference | TrackFrameKey::Group { .. } => None,
    }
}

pub(super) fn begin_track_rename(frame: &TrackFrame) -> Option<TrackRenameState> {
    Some(TrackRenameState {
        key: frame.key,
        pane: frame.channel,
        draft: editable_track_name(frame)?.to_owned(),
        request_focus: true,
    })
}

pub(super) fn alignment_subtrack_has_independent_visibility(
    frame: &TrackFrame,
    subtrack: AlignmentSubtrack,
) -> bool {
    // A read-pane frame is already the independently visible Reads node. Its
    // sole subtrack is status/content for that node, not another visibility
    // owner. Annotation subtracks remain independently toggleable.
    !(frame.channel == TrackPane::Reads && subtrack == AlignmentSubtrack::Reads)
}

/// Presentation metadata for an alignment-track group. Groups deliberately
/// remain separate from read grouping and quantitative overlay composition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrackGroupChrome {
    pub id: u32,
    pub pane: TrackPane,
    pub anchor: TrackId,
    pub members: Vec<TrackId>,
    pub collapsed: bool,
    pub content_visible: bool,
}

impl TrackGroupChrome {
    pub(super) fn contains(&self, track_id: TrackId) -> bool {
        self.members.contains(&track_id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackUiAction {
    ToggleSection(TrackPane),
    SetPaneRowsExpanded(TrackPane, bool),
    ResetPaneRowHeights(TrackPane),
    Select(TrackFrameKey, TrackPane),
    OpenSettings(TrackFrameKey),
    RenameTrack(TrackFrameKey, String),
    ToggleVisibility(TrackFrameKey),
    SetNodeVisibility(PresentationNodeId, bool),
    ToggleCollapsed(TrackFrameKey),
    ToggleCompositeVisibility(TrackFrameKey),
    ToggleAlignmentSubtrack(TrackId, AlignmentSubtrack),
    ToggleCompositeMember(TrackId),
    GroupSelectedTracks(TrackPane),
    ToggleTrackGroup(TrackPane, u32),
    ToggleTrackGroupVisibility(TrackPane, u32),
    UngroupTracks(TrackPane, u32),
    Export(TrackFrameKey),
    Remove(TrackFrameKey),
    ReorderDropped(PresentationNodeId, f32),
    ReorderKeyboard(PresentationNodeId, i32),
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
    /// Exact same-frame hitbox of the shared annotation/read pane splitter.
    /// Native canvas gestures consume this rectangle to avoid starting a
    /// genomic pan underneath the egui resize interaction.
    pub pane_divider_rect: Option<Rect>,
}

/// Fixed workspace section chrome. Content bounds are supplied by the display
/// layout so the section headers, clipping, gutter controls, and scrollbars all
/// consume the same geometry revision.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrackSectionChrome {
    pub annotation_expanded: bool,
    pub reads_expanded: bool,
    pub annotation_track_count: usize,
    pub read_track_count: usize,
    pub annotation_content_top: f32,
    pub read_content_top: f32,
}

impl Default for TrackSectionChrome {
    fn default() -> Self {
        Self {
            annotation_expanded: true,
            reads_expanded: true,
            annotation_track_count: 0,
            read_track_count: 0,
            annotation_content_top: 0.0,
            read_content_top: 0.0,
        }
    }
}

pub(super) const GUTTER_TEXT_GAP: f32 = 6.0;

pub(super) const MAX_GUTTER_LABEL_FRACTION: f32 = 0.55;

pub(super) const ANNOTATION_PANE_GUTTER_COLOR: Color32 = Color32::from_rgb(234, 241, 248);

pub(super) const READ_PANE_GUTTER_COLOR: Color32 = Color32::from_gray(249);

pub(super) fn gutter_row_widths(
    label_width: f32,
    detail_width: f32,
    available_width: f32,
) -> (f32, f32) {
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

pub(super) fn layout_elided_text(
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

pub(super) fn paint_elided_text(
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
