//! Device-free lane layout of one panel, in logical points.
//!
//! The desktop computes its pane layout with the widget toolkit's rectangle type; settlement,
//! scene-request dispatch and presentation read only this plain copy of it, so they run the same
//! way in a headless session, whose host lays out lanes from an image size instead of a window.
//! The geometry is the desktop's resolved, scrolled layout: a lane rectangle here is exactly the
//! rectangle the desktop clips, hit-tests and draws with.

use rgv_core::{PanelId, TrackChannel, TrackId, TrackKind};

/// An axis-aligned rectangle in logical points, with `left <= right` and `top <= bottom` for a
/// non-empty rectangle. [`LayoutRect::NOTHING`] is the empty rectangle (inverted infinite
/// bounds), matching the desktop's empty rectangle so widths and heights compare the same way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutRect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl LayoutRect {
    pub const NOTHING: Self = Self {
        left: f32::INFINITY,
        top: f32::INFINITY,
        right: f32::NEG_INFINITY,
        bottom: f32::NEG_INFINITY,
    };

    pub const fn from_min_max(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    pub fn width(self) -> f32 {
        self.right - self.left
    }

    pub fn height(self) -> f32 {
        self.bottom - self.top
    }
}

/// The stable semantic identity of one alignment-owned sublane. Display labels are free text
/// and never decide what a sublane is.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SublaneKind {
    Reference,
    Coverage,
    Sashimi,
    Modifications,
    Reads,
}

/// Who owns a laid-out lane.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LaneOwner {
    Reference,
    Group { channel: TrackChannel, id: u32 },
    Alignment(TrackId),
    Auxiliary(TrackId),
}

/// One row inside a lane, in unscrolled pane coordinates (subtract the pane's
/// [`PaneLanes::scroll_points`] for an annotation-pane row on screen).
#[derive(Clone, Debug, PartialEq)]
pub struct Sublane {
    /// `Some` for alignment-owned rows; `None` for informational and composite-member rows.
    pub kind: Option<SublaneKind>,
    pub label: String,
    pub top: f32,
    pub bottom: f32,
    /// A hidden sublane kept as a compact placeholder.
    pub collapsed: bool,
}

/// One track or group lane, resolved and scrolled.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneLayout {
    pub owner: LaneOwner,
    pub channel: TrackChannel,
    pub name: String,
    pub track_kind: TrackKind,
    /// Unscrolled frame extent in pane coordinates.
    pub frame_top: f32,
    pub frame_bottom: f32,
    pub row_rect: LayoutRect,
    pub header_rect: LayoutRect,
    pub content_rect: Option<LayoutRect>,
    pub clip_rect: LayoutRect,
    pub effective_content_visible: bool,
    pub sublanes: Vec<Sublane>,
}

impl LaneLayout {
    /// Whether this lane is the alignment track `track_id` in `channel`.
    pub fn is_alignment(&self, track_id: TrackId, channel: TrackChannel) -> bool {
        self.owner == LaneOwner::Alignment(track_id) && self.channel == channel
    }
}

/// One vertical pane (annotation or reads) of a panel.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneLanes {
    pub channel: TrackChannel,
    pub pane_rect: LayoutRect,
    pub content_bottom: f32,
    pub scroll_points: f32,
    pub stack_scroll_points: f32,
    pub lanes: Vec<LaneLayout>,
}

impl PaneLanes {
    pub const fn empty(channel: TrackChannel) -> Self {
        Self {
            channel,
            pane_rect: LayoutRect::NOTHING,
            content_bottom: 0.0,
            scroll_points: 0.0,
            stack_scroll_points: 0.0,
            lanes: Vec::new(),
        }
    }

    /// The lane of alignment track `track_id`, if this pane lays one out.
    pub fn alignment_lane(&self, track_id: TrackId) -> Option<&LaneLayout> {
        self.lanes
            .iter()
            .find(|lane| lane.is_alignment(track_id, self.channel))
    }
}

/// The committed lane layout of one panel: its canvas column, the track gutter beside it, and
/// both panes.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelLaneLayout {
    pub panel_id: PanelId,
    /// The panel's genomic canvas column. Lane rectangles share its horizontal extent.
    pub canvas: LayoutRect,
    /// Width of the track-name gutter to the left of the canvases.
    pub gutter_width: f32,
    pub annotation: PaneLanes,
    pub reads: PaneLanes,
}

impl PanelLaneLayout {
    /// An empty layout over `canvas`: no lanes in either pane.
    pub const fn empty(panel_id: PanelId, canvas: LayoutRect) -> Self {
        Self {
            panel_id,
            canvas,
            gutter_width: 0.0,
            annotation: PaneLanes::empty(TrackChannel::Annotation),
            reads: PaneLanes::empty(TrackChannel::Reads),
        }
    }

    pub const fn pane(&self, channel: TrackChannel) -> &PaneLanes {
        match channel {
            TrackChannel::Annotation => &self.annotation,
            TrackChannel::Reads => &self.reads,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_has_negative_extent() {
        assert!(LayoutRect::NOTHING.width() < 0.0);
        assert!(LayoutRect::NOTHING.height() < 0.0);
        let rect = LayoutRect::from_min_max(1.0, 2.0, 11.0, 7.0);
        assert!((rect.width() - 10.0).abs() < f32::EPSILON);
        assert!((rect.height() - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn alignment_lane_matches_track_and_pane() {
        let lane = LaneLayout {
            owner: LaneOwner::Alignment(TrackId::new(3)),
            channel: TrackChannel::Reads,
            name: "reads".to_owned(),
            track_kind: TrackKind::Alignment,
            frame_top: 0.0,
            frame_bottom: 10.0,
            row_rect: LayoutRect::from_min_max(0.0, 0.0, 10.0, 10.0),
            header_rect: LayoutRect::NOTHING,
            content_rect: None,
            clip_rect: LayoutRect::from_min_max(0.0, 0.0, 10.0, 10.0),
            effective_content_visible: true,
            sublanes: Vec::new(),
        };
        let mut pane = PaneLanes::empty(TrackChannel::Reads);
        pane.lanes.push(lane);
        assert!(pane.alignment_lane(TrackId::new(3)).is_some());
        assert!(pane.alignment_lane(TrackId::new(4)).is_none());
        let mut annotation = pane.clone();
        annotation.channel = TrackChannel::Annotation;
        assert!(annotation.alignment_lane(TrackId::new(3)).is_none());
    }
}
