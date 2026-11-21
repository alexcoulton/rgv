use egui::{Rect, pos2};
use rgv_core::{PresentationNodeId, TrackChannel, TrackId, TrackKind};

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
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackSubtrack {
    /// Stable semantic identity for alignment-owned lanes. Display labels remain freely mutable
    /// presentation text and must never determine visibility actions or lane ownership.
    pub alignment_subtrack: Option<AlignmentSubtrack>,
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

pub(super) fn clipped_or_empty_at_edge(rect: Rect, clip: Rect) -> Rect {
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
