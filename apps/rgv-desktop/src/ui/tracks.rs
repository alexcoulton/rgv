use std::sync::Arc;

use egui::{
    Align2, Color32, FontId, Galley, Rect, Sense, Stroke, Ui, pos2,
    text::{LayoutJob, TextFormat, TextWrapping},
};
use rgv_core::{PanelId, PresentationNodeId, TrackChannel, TrackId, TrackKind};
use rgv_layout::{DisplayList, TextPrimitive};

use super::CanvasGeometry;
use super::icons::{AppIcon, paint_icon};
use crate::{MAX_VERTICAL_DENSITY, MIN_VERTICAL_DENSITY};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TrackFrameKey {
    Reference,
    Group { channel: TrackChannel, id: u32 },
    Alignment(TrackId),
    Auxiliary(TrackId),
}

/// The visual section that owns a track-frame interaction. Alignment
/// annotation and read frames intentionally share a track key, so the key
/// alone cannot identify which row the user selected.
pub type TrackPane = TrackChannel;

pub const TRACK_HEADER_HEIGHT: f32 = 22.0;
pub const TRACK_SECTION_HEADER_HEIGHT: f32 = 26.0;

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
    /// Stable presentation identity. This must never be recovered from frame
    /// geometry such as `frozen_bottom`.
    pub channel: TrackPane,
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

/// Immutable geometry and visibility resolved for one presentation node. The
/// same item is consumed by canvas clipping, gutter chrome, hit testing, and
/// automation; those consumers must not reconstruct pane identity or parent
/// visibility from render primitives.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneItemLayout {
    pub id: PresentationNodeId,
    pub frame: TrackFrame,
    pub row_rect: Rect,
    pub header_rect: Rect,
    pub content_rect: Option<Rect>,
    pub clip_rect: Rect,
    pub desired_content_visible: bool,
    pub effective_content_visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneLayoutSnapshot {
    pub channel: TrackPane,
    pub pane_rect: Rect,
    pub content_bottom: f32,
    pub scroll_points: f32,
    pub stack_scroll_points: f32,
    pub items: Vec<PaneItemLayout>,
}

impl PaneLayoutSnapshot {
    pub fn empty(channel: TrackPane) -> Self {
        Self {
            channel,
            pane_rect: Rect::NOTHING,
            content_bottom: 0.0,
            scroll_points: 0.0,
            stack_scroll_points: 0.0,
            items: Vec::new(),
        }
    }

    /// Reprojects every canonical pane row into the current scrolled viewport.
    ///
    /// `items` deliberately retains rows that are fully clipped. Keeping the
    /// complete ordered tree here means a fast scroll can expose a row without
    /// requiring a renderer rebuild, while the zero-height projected rectangles
    /// keep clipped rows out of hit testing and accessibility geometry.
    pub fn reproject_scroll(&mut self, scroll_points: f32) {
        self.scroll_points = scroll_points;
        let offset = if self.channel == TrackPane::Annotation {
            scroll_points
        } else {
            0.0
        };
        for item in &mut self.items {
            let natural_row = Rect::from_min_max(
                pos2(self.pane_rect.left(), item.frame.top - offset),
                pos2(self.pane_rect.right(), item.frame.bottom - offset),
            );
            item.row_rect = clipped_or_empty_at_edge(natural_row, self.pane_rect);
            item.clip_rect = item.row_rect;

            let natural_header = Rect::from_min_max(
                natural_row.min,
                pos2(
                    natural_row.right(),
                    (natural_row.top() + TRACK_HEADER_HEIGHT).min(natural_row.bottom()),
                ),
            );
            item.header_rect = clipped_or_empty_at_edge(natural_header, item.row_rect);
            item.content_rect = (!matches!(item.id, PresentationNodeId::Group { .. })
                && item.effective_content_visible)
                .then(|| {
                    let natural_content = Rect::from_min_max(
                        pos2(natural_row.left(), natural_header.bottom()),
                        natural_row.max,
                    );
                    clipped_or_empty_at_edge(natural_content, item.row_rect)
                })
                .filter(|rect| rect.height() > 0.0);
        }
    }
}

fn clipped_or_empty_at_edge(rect: Rect, clip: Rect) -> Rect {
    if rect.bottom() <= clip.top() {
        Rect::from_min_max(
            pos2(clip.left(), clip.top()),
            pos2(clip.right(), clip.top()),
        )
    } else if rect.top() >= clip.bottom() {
        Rect::from_min_max(
            pos2(clip.left(), clip.bottom()),
            pos2(clip.right(), clip.bottom()),
        )
    } else {
        rect.intersect(clip)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspacePaneLayout {
    pub annotation: PaneLayoutSnapshot,
    pub reads: PaneLayoutSnapshot,
}

impl Default for WorkspacePaneLayout {
    fn default() -> Self {
        Self {
            annotation: PaneLayoutSnapshot::empty(TrackPane::Annotation),
            reads: PaneLayoutSnapshot::empty(TrackPane::Reads),
        }
    }
}

impl WorkspacePaneLayout {
    pub fn for_channel(&self, channel: TrackPane) -> &PaneLayoutSnapshot {
        match channel {
            TrackPane::Annotation => &self.annotation,
            TrackPane::Reads => &self.reads,
        }
    }

    pub fn frames(&self) -> impl Iterator<Item = &TrackFrame> {
        self.annotation
            .items
            .iter()
            .chain(self.reads.items.iter())
            .map(|item| &item.frame)
    }
}

fn presentation_node_for_frame(frame: &TrackFrame) -> Option<PresentationNodeId> {
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

fn group_chevron_segments(rect: Rect, collapsed: bool) -> [[egui::Pos2; 2]; 2] {
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

fn track_header_label_geometry(
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

const READ_TRACK_NAME_PREFIX: &str = "Reads · ";

#[derive(Clone, Debug, Eq, PartialEq)]
struct TrackRenameState {
    key: TrackFrameKey,
    pane: TrackPane,
    draft: String,
    request_focus: bool,
}

fn track_rename_state_id() -> egui::Id {
    egui::Id::new("track-inline-rename-state")
}

fn track_rename_editor_id(key: TrackFrameKey, pane: TrackPane) -> egui::Id {
    egui::Id::new(("track-inline-rename-editor", key, pane))
}

fn editable_track_name(frame: &TrackFrame) -> Option<&str> {
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

fn begin_track_rename(frame: &TrackFrame) -> Option<TrackRenameState> {
    Some(TrackRenameState {
        key: frame.key,
        pane: frame.channel,
        draft: editable_track_name(frame)?.to_owned(),
        request_focus: true,
    })
}

fn alignment_subtrack_has_independent_visibility(
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
    fn contains(&self, track_id: TrackId) -> bool {
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

fn paint_annotation_scrollbar_with_id(
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
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn paint_vertical_scrollbar_with_id(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    maximum: f32,
    offset: &mut f32,
    density: f32,
    camera_viewport_height: f32,
    preview_scroll_fraction: Option<f32>,
    overview: Option<&ScrollbarOverview>,
) -> ScrollbarInteraction {
    let maximum = maximum.max(0.0);
    *offset = offset.clamp(0.0, maximum);
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

#[allow(clippy::too_many_arguments)]
fn paint_track_section_header(
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
fn paint_track_viewport_chrome(
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
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
    let mut result = TrackGutterResult::default();
    let rename_state_id = track_rename_state_id();
    let mut rename_state = ui
        .ctx()
        .data(|data| data.get_temp::<TrackRenameState>(rename_state_id));
    let painter = ui.painter().clone();
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
        &painter,
        geometry.gutter,
        annotation_header,
        TrackPane::Annotation,
        sections.annotation_expanded,
        sections.annotation_track_count,
        &mut result,
    );
    paint_track_section_header(
        ui,
        &painter,
        geometry.gutter,
        read_header,
        TrackPane::Reads,
        sections.reads_expanded,
        sections.read_track_count,
        &mut result,
    );

    for frame in frames {
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
            continue;
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
            rename_state = begin_track_rename(frame);
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
                rename_state = begin_track_rename(frame);
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
                rename_state = begin_track_rename(frame);
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
            rename_state = None;
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
                TrackFrameKey::Alignment(_) => AlignmentSubtrack::from_label(&subtrack.label),
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
                    continue;
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
                    continue;
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
    result
}

fn boundary_inside_clip(boundary: f32, clip_top: f32, clip_bottom: f32) -> bool {
    boundary >= clip_top && boundary <= clip_bottom
}

fn frame_screen_bounds(frame: &TrackFrame, vertical_scroll: f32) -> (f32, f32) {
    (frame.top - vertical_scroll, frame.bottom - vertical_scroll)
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg(test)]
struct VerticalScreenBounds {
    top: f32,
    bottom: f32,
}

#[cfg(test)]
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

    fn read_frame_with_status(track_id: TrackId) -> TrackFrame {
        TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Reads,
            name: "Reads · Tumour paired".to_owned(),
            kind: TrackKind::Alignment,
            source_label: Some("tumour-paired.bam".to_owned()),
            top: 20.0,
            bottom: 180.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(42.0),
            subtracks: vec![TrackSubtrack {
                label: "Reads".to_owned(),
                detail: Some("159 visible · 211 loaded".to_owned()),
                top: 42.0,
                bottom: 180.0,
                collapsed: false,
                composite_member: None,
            }],
        }
    }

    #[test]
    fn read_status_row_does_not_own_a_second_visibility_control() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let frame = read_frame_with_status(TrackId::new(9));
        assert!(!alignment_subtrack_has_independent_visibility(
            &frame,
            AlignmentSubtrack::Reads,
        ));

        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 240.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                let geometry = CanvasGeometry::from_viewport(ui.max_rect());
                let _ = paint_track_viewport_chrome(
                    ui,
                    geometry,
                    std::slice::from_ref(&frame),
                    0.0,
                    0.0,
                    0.0,
                    geometry.viewport.top(),
                    None,
                    &[],
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let visibility_labels = update
            .nodes
            .iter()
            .filter_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::CheckBox)
                    .then(|| node.label())
                    .flatten()
            })
            .filter(|label| label.contains("read content") || label.contains("Reads subtrack"))
            .collect::<Vec<_>>();

        assert_eq!(
            visibility_labels,
            vec!["Reads · Tumour paired read content visible"]
        );
    }

    #[test]
    fn read_status_eye_column_is_non_interactive() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 240.0));
        let frame = read_frame_with_status(TrackId::new(9));
        let pointer = pos2(11.0, 51.0);
        let render = |ui: &mut Ui| {
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            paint_track_viewport_chrome(
                ui,
                geometry,
                std::slice::from_ref(&frame),
                0.0,
                0.0,
                0.0,
                geometry.viewport.top(),
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

        assert!(actions.is_empty());
    }

    #[test]
    fn read_track_inline_rename_edits_the_shared_source_name() {
        let frame = read_frame_with_status(TrackId::new(9));
        let state = begin_track_rename(&frame).unwrap();

        assert_eq!(state.key, TrackFrameKey::Alignment(TrackId::new(9)));
        assert_eq!(state.pane, TrackPane::Reads);
        assert_eq!(state.draft, "Tumour paired");
        assert!(
            editable_track_name(&TrackFrame {
                key: TrackFrameKey::Reference,
                channel: TrackPane::Annotation,
                name: "Reference sequence".to_owned(),
                ..frame
            })
            .is_none()
        );
    }

    #[test]
    fn double_clicking_read_track_name_begins_inline_rename() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 240.0));
        let frame = read_frame_with_status(TrackId::new(9));
        let pointer = pos2(80.0, 31.0);
        let render = |input: egui::RawInput| {
            context.run_ui(input, |ui| {
                let geometry = CanvasGeometry::from_viewport(ui.max_rect());
                let _ = paint_track_viewport_chrome(
                    ui,
                    geometry,
                    std::slice::from_ref(&frame),
                    0.0,
                    0.0,
                    0.0,
                    geometry.viewport.top(),
                    None,
                    &[],
                );
            })
        };
        let input = |time, events| egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(time),
            events,
            ..egui::RawInput::default()
        };

        let _ = render(input(0.0, vec![egui::Event::PointerMoved(pointer)]));
        for (time, pressed) in [(0.10, true), (0.11, false), (0.20, true), (0.21, false)] {
            let _ = render(input(
                time,
                vec![egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            ));
        }

        let state = context
            .data(|data| data.get_temp::<TrackRenameState>(track_rename_state_id()))
            .unwrap();
        assert_eq!(state.key, TrackFrameKey::Alignment(TrackId::new(9)));
        assert_eq!(state.pane, TrackPane::Reads);
        assert_eq!(state.draft, "Tumour paired");
    }

    #[test]
    fn enter_commits_inline_rename_as_one_canonical_action() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 240.0));
        let frame = read_frame_with_status(TrackId::new(9));
        context.data_mut(|data| {
            data.insert_temp(
                track_rename_state_id(),
                TrackRenameState {
                    key: frame.key,
                    pane: frame.channel,
                    draft: "  Relapse tumour  ".to_owned(),
                    request_focus: true,
                },
            );
        });
        let render = |input: egui::RawInput, actions: &mut Vec<TrackUiAction>| {
            context.run_ui(input, |ui| {
                let geometry = CanvasGeometry::from_viewport(ui.max_rect());
                *actions = paint_track_viewport_chrome(
                    ui,
                    geometry,
                    std::slice::from_ref(&frame),
                    0.0,
                    0.0,
                    0.0,
                    geometry.viewport.top(),
                    None,
                    &[],
                )
                .actions;
            })
        };

        let mut actions = Vec::new();
        let _ = render(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            &mut actions,
        );
        let _ = render(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..egui::RawInput::default()
            },
            &mut actions,
        );

        assert_eq!(
            actions,
            vec![TrackUiAction::RenameTrack(
                TrackFrameKey::Alignment(TrackId::new(9)),
                "Relapse tumour".to_owned(),
            )]
        );
        assert!(
            context
                .data(|data| data.get_temp::<TrackRenameState>(track_rename_state_id()))
                .is_none()
        );
    }

    #[test]
    fn escape_cancels_inline_rename_without_an_action() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 240.0));
        let frame = read_frame_with_status(TrackId::new(9));
        context.data_mut(|data| {
            data.insert_temp(
                track_rename_state_id(),
                TrackRenameState {
                    key: frame.key,
                    pane: frame.channel,
                    draft: "Uncommitted name".to_owned(),
                    request_focus: true,
                },
            );
        });
        let render = |input: egui::RawInput, actions: &mut Vec<TrackUiAction>| {
            context.run_ui(input, |ui| {
                let geometry = CanvasGeometry::from_viewport(ui.max_rect());
                *actions = paint_track_viewport_chrome(
                    ui,
                    geometry,
                    std::slice::from_ref(&frame),
                    0.0,
                    0.0,
                    0.0,
                    geometry.viewport.top(),
                    None,
                    &[],
                )
                .actions;
            })
        };

        let mut actions = Vec::new();
        let _ = render(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            &mut actions,
        );
        let _ = render(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..egui::RawInput::default()
            },
            &mut actions,
        );

        assert!(actions.is_empty());
        assert!(
            context
                .data(|data| data.get_temp::<TrackRenameState>(track_rename_state_id()))
                .is_none()
        );
    }

    fn frame(id: u64, top: f32) -> TrackFrame {
        TrackFrame {
            key: TrackFrameKey::Auxiliary(TrackId::new(id)),
            channel: TrackPane::Annotation,
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
            channel: TrackPane::Reads,
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
            channel: TrackPane::Reads,
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
    fn split_workspaces_have_distinct_accessible_read_scrollbars() {
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
                let mut first_offset = 100.0;
                let _ = paint_read_panel_scrollbar(
                    ui,
                    Rect::from_min_max(pos2(10.0, 100.0), pos2(32.0, 300.0)),
                    PanelId::new(1),
                    TrackId::new(11),
                    1_000.0,
                    &mut first_offset,
                    1.0,
                    200.0,
                    None,
                    None,
                );
                let mut second_offset = 700.0;
                let _ = paint_read_panel_scrollbar(
                    ui,
                    Rect::from_min_max(pos2(68.0, 100.0), pos2(90.0, 300.0)),
                    PanelId::new(2),
                    TrackId::new(11),
                    1_000.0,
                    &mut second_offset,
                    0.5,
                    200.0,
                    None,
                    None,
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let read_sliders = update
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.role() == egui::accesskit::Role::Slider
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Read stack overview"))
            })
            .collect::<Vec<_>>();
        assert_eq!(read_sliders.len(), 2);
        assert!(
            read_sliders
                .iter()
                .any(|(_, node)| node.numeric_value() == Some(10.0))
        );
        assert!(
            read_sliders
                .iter()
                .any(|(_, node)| node.numeric_value() == Some(70.0))
        );
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
                    &[TrackId::new(7)],
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
                        channel: TrackPane::Annotation,
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
                        channel: TrackPane::Reads,
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
    fn clicking_read_frame_selects_the_read_pane_not_its_annotation_frame() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [
            TrackFrame {
                key: TrackFrameKey::Alignment(track_id),
                channel: TrackPane::Annotation,
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
                channel: TrackPane::Reads,
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
        let pointer = pos2(50.0, 250.0);

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
            [TrackUiAction::Select(
                TrackFrameKey::Alignment(track_id),
                TrackPane::Reads,
            )]
        );
    }

    #[test]
    fn read_frame_selection_does_not_highlight_the_matching_annotation_frame() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let track_id = TrackId::new(9);
        let frames = [
            TrackFrame {
                key: TrackFrameKey::Alignment(track_id),
                channel: TrackPane::Annotation,
                name: "Annotation · exact-bases.bam".to_owned(),
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
                channel: TrackPane::Reads,
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
                let _ = paint_track_viewport_chrome_with_groups(
                    ui,
                    geometry,
                    &frames,
                    0.0,
                    0.0,
                    0.0,
                    180.0,
                    Some(TrackFrameKey::Alignment(track_id)),
                    &[],
                    &[track_id],
                    &[],
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let annotation = &update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Annotation · exact-bases.bam "))
                    && node
                        .label()
                        .is_some_and(|label| label.contains("Alignment track;"))
            })
            .unwrap()
            .1;
        let reads = &update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Button
                    && node
                        .label()
                        .is_some_and(|label| label.starts_with("Reads · exact-bases.bam "))
                    && node
                        .label()
                        .is_some_and(|label| label.contains("Alignment track;"))
            })
            .unwrap()
            .1;

        assert_eq!(annotation.toggled(), Some(egui::accesskit::Toggled::False));
        assert_eq!(reads.toggled(), Some(egui::accesskit::Toggled::True));
    }

    #[test]
    fn annotation_track_body_context_menu_stays_open_and_offers_grouping() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [
            TrackFrame {
                key: TrackFrameKey::Alignment(track_id),
                channel: TrackPane::Annotation,
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
                channel: TrackPane::Reads,
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
                &[track_id, TrackId::new(10)],
            )
        };
        // This is deliberately below the 22-pixel header, in the annotation
        // track body where users naturally right-click coverage or summaries.
        let pointer = pos2(50.0, 80.0);

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

        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..egui::RawInput::default()
            },
            |ui| {
                let _ = render(ui);
            },
        );
        assert!(egui::Popup::is_id_open(&context, popup_id));
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node
                    .label()
                    .is_some_and(|label| label == "Group 2 selected tracks")
        }));
    }

    #[test]
    fn read_track_context_menu_offers_grouping_for_a_read_range_selection() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Reads,
            name: "Reads · exact-bases.bam".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 180.0,
            bottom: 420.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(202.0),
            subtracks: Vec::new(),
        }];
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome_with_groups(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                180.0,
                Some(TrackFrameKey::Alignment(track_id)),
                &[],
                &[track_id, TrackId::new(10)],
                &[],
            )
        };
        let pointer = pos2(50.0, 250.0);

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
        let output = context.run_ui(
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

        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node
                    .label()
                    .is_some_and(|label| label == "Group 2 selected tracks")
        }));
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
                TrackUiAction::Select(
                    TrackFrameKey::Auxiliary(TrackId::new(7)),
                    TrackPane::Annotation,
                ),
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
            vec![TrackUiAction::SetNodeVisibility(
                PresentationNodeId::Track {
                    channel: TrackChannel::Annotation,
                    track_id: TrackId::new(7),
                },
                false,
            )]
        );
    }

    #[test]
    fn group_header_eye_toggles_group_content_without_changing_expansion() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let frames = [TrackFrame {
            key: TrackFrameKey::Group {
                channel: TrackPane::Annotation,
                id: 4,
            },
            channel: TrackPane::Annotation,
            name: "Group 4 · 2 tracks".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 20.0,
            bottom: 46.0,
            visible: true,
            collapsed: true,
            frozen_bottom: None,
            subtracks: Vec::new(),
        }];
        let groups = [TrackGroupChrome {
            id: 4,
            pane: TrackPane::Annotation,
            anchor: TrackId::new(7),
            members: vec![TrackId::new(7), TrackId::new(8)],
            collapsed: false,
            content_visible: true,
        }];
        let pointer = pos2(11.0, 31.0);
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome_with_groups(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                ui.max_rect().bottom(),
                None,
                &[],
                &[],
                &groups,
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
            vec![TrackUiAction::ToggleTrackGroupVisibility(
                TrackPane::Annotation,
                4
            )]
        );
    }

    #[test]
    fn group_chevrons_are_vector_geometry_and_labels_start_after_the_toggle() {
        let label_rect = Rect::from_min_size(pos2(10.0, 20.0), egui::vec2(168.0, 22.0));
        let toggle_rect = Rect::from_center_size(pos2(40.0, 31.0), egui::vec2(16.0, 16.0));
        let collapsed = group_chevron_segments(toggle_rect, true);
        let expanded = group_chevron_segments(toggle_rect, false);

        assert!(collapsed[0][1].x > collapsed[0][0].x);
        assert_eq!(collapsed[0][1], collapsed[1][0]);
        assert!(expanded[0][1].y > expanded[0][0].y);
        assert_eq!(expanded[0][1], expanded[1][0]);
        assert!(
            collapsed
                .into_iter()
                .flatten()
                .all(|point| toggle_rect.contains(point))
        );
        assert!(
            expanded
                .into_iter()
                .flatten()
                .all(|point| toggle_rect.contains(point))
        );

        let (group_position, group_width) =
            track_header_label_geometry(label_rect, label_rect.top(), true);
        let (track_position, track_width) =
            track_header_label_geometry(label_rect, label_rect.top(), false);
        assert!(group_position.x >= toggle_rect.right() + 2.0);
        assert_eq!(track_position.x, label_rect.left() + 22.0);
        assert_eq!(group_width, 124.0);
        assert_eq!(track_width, 115.0);
    }

    #[test]
    fn clicking_a_hidden_track_eye_requests_visibility_restore() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let mut hidden = frame(7, 20.0);
        hidden.bottom = hidden.top + 26.0;
        hidden.visible = false;
        let frames = [hidden];
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
            vec![TrackUiAction::SetNodeVisibility(
                PresentationNodeId::Track {
                    channel: TrackChannel::Annotation,
                    track_id: TrackId::new(7),
                },
                true,
            )]
        );
    }

    #[test]
    fn clicking_read_pane_eye_toggles_reads_instead_of_the_whole_track() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Reads,
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
            vec![TrackUiAction::SetNodeVisibility(
                PresentationNodeId::Track {
                    channel: TrackChannel::Reads,
                    track_id,
                },
                true,
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
                    channel: TrackPane::Annotation,
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
            channel: TrackPane::Annotation,
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
            channel: TrackPane::Annotation,
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
            channel: TrackPane::Reads,
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
    fn a_single_read_panel_has_no_lower_resize_boundary() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let track_id = TrackId::new(9);
        let frames = [TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: TrackPane::Reads,
            name: "Reads · sample A".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: 200.0,
            bottom: 500.0,
            visible: true,
            collapsed: false,
            frozen_bottom: Some(222.0),
            subtracks: vec![TrackSubtrack {
                label: "Reads".to_owned(),
                detail: None,
                top: 222.0,
                bottom: 500.0,
                collapsed: false,
                composite_member: None,
            }],
        }];
        let pointer = pos2(50.0, 499.0);
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome_with_sections(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &frames,
                0.0,
                0.0,
                0.0,
                200.0,
                None,
                &[],
                &[],
                &[],
                TrackSectionChrome {
                    annotation_expanded: true,
                    reads_expanded: true,
                    annotation_track_count: 0,
                    read_track_count: 1,
                    annotation_content_top: 0.0,
                    read_content_top: 200.0,
                },
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

        assert!(!actions.iter().any(|action| matches!(
            action,
            TrackUiAction::ResizeReadTrackStarted(id, _) if *id == track_id
        )));
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
    fn pane_divider_owns_drag_above_the_read_section_header() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome_with_sections(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &[],
                0.0,
                0.0,
                26.0,
                250.0,
                None,
                &[],
                &[],
                &[],
                TrackSectionChrome {
                    annotation_expanded: true,
                    reads_expanded: true,
                    annotation_track_count: 1,
                    read_track_count: 1,
                    annotation_content_top: 26.0,
                    read_content_top: 276.0,
                },
            )
        };
        // The lower half of the splitter overlaps the painted Read data
        // section header. That overlap must belong to the splitter on press.
        let pointer = pos2(400.0, 252.0);
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
                events: vec![egui::Event::PointerMoved(pos2(400.0, 262.0))],
                ..egui::RawInput::default()
            },
            |ui| actions = render(ui).actions,
        );
        assert_eq!(actions, vec![TrackUiAction::ResizeReadPaneDragged(10.0)]);
    }

    #[test]
    fn pane_divider_remains_draggable_above_the_canvas_accessibility_surface() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let render = |ui: &mut Ui| {
            let geometry = CanvasGeometry::from_viewport(ui.max_rect());
            let result =
                paint_track_viewport_chrome(ui, geometry, &[], 0.0, 0.0, 0.0, 250.0, None, &[]);
            ui.interact(
                geometry.genomic,
                egui::Id::new("test-genomic-canvas-accessibility"),
                egui::Sense::focusable_noninteractive(),
            );
            result
        };
        let pointer = pos2(400.0, 250.0);
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
    }

    #[test]
    fn alignment_read_content_supports_a_panel_local_scroll_offset() {
        let frame = TrackFrame {
            key: TrackFrameKey::Alignment(TrackId::new(1)),
            channel: TrackPane::Reads,
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
    fn screen_to_content_mapping_accepts_a_panel_local_scroll_offset() {
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
            "a read group maps through its panel-local scroll"
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

    #[test]
    fn fixed_section_header_toggles_without_targeting_a_child_row() {
        let context = egui::Context::default();
        let screen_rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 500.0));
        let render = |ui: &mut Ui| {
            paint_track_viewport_chrome_with_sections(
                ui,
                CanvasGeometry::from_viewport(ui.max_rect()),
                &[],
                0.0,
                0.0,
                26.0,
                180.0,
                None,
                &[],
                &[],
                &[],
                TrackSectionChrome {
                    annotation_expanded: true,
                    reads_expanded: true,
                    annotation_track_count: 4,
                    read_track_count: 2,
                    annotation_content_top: 26.0,
                    read_content_top: 206.0,
                },
            )
        };
        let pointer = pos2(300.0, 13.0);

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
            [TrackUiAction::ToggleSection(TrackPane::Annotation)]
        );
    }
}
