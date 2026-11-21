//! The action model: every state-changing user intent as one typed value.
//!
//! The window (menus, shortcuts, pointer gestures, dialogs) and the automation protocol both
//! express what they want as an [`Action`] and hand it to the host's `apply_action`. There is one
//! implementation of each intent, so the two cannot drift apart, and the undo history records
//! what either of them did.
//!
//! Nothing here names a window, GPU or widget type. [`reduce`] is the headless half: it edits
//! the panel workspace and returns the [`Effect`]s the host must carry out (queries, layout,
//! redraw). That split is what lets the reducer and this vocabulary move into a session crate
//! without the desktop shell.

pub mod history;
pub mod reduce;
#[cfg(test)]
mod tests;
pub mod translate;

use rgv_automation::{AutomationHistoryNavigation, AutomationViewportOperation};
use rgv_core::{
    GenomicInterval, NavigationEntry, PanelId, RegionOfInterest, ReviewItem, SessionAlignmentTrack,
    TrackChangeSet, TrackConfig, TrackId, TrackIntent, Viewport, WorkspacePresentation,
};
use std::collections::BTreeSet;

/// How a viewport change is committed to history and to the data pipeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewportCommit {
    /// One complete navigation: recorded in the panel's history at once.
    Discrete,
    /// One repeatable step (key repeat, toolbar zoom). History gets one entry when the burst
    /// settles.
    Step,
    /// One sample of a continuous pointer gesture (drag, wheel, pinch).
    Gesture,
}

/// A panel as an undo of `ClosePanel` needs it: identity, place and locus.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelRestore {
    pub panel_id: PanelId,
    /// Position in the left-to-right panel order.
    pub index: usize,
    /// Where the panel was looking, by contig name (0-based half-open).
    pub locus: NavigationEntry,
    pub contig_length: u64,
    pub requested_locus: Option<String>,
    pub was_focused: bool,
}

/// Removed tracks as an undo of `RemoveTracks` needs them: configuration only. No engine,
/// cache or file handle is kept alive by an undo entry; reopening starts new engines.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackRestore {
    /// Alignment tracks with their position in the alignment order.
    pub alignment: Vec<(usize, SessionAlignmentTrack)>,
    /// Annotation and signal tracks with their position in the track list.
    pub auxiliary: Vec<(usize, TrackConfig)>,
    /// The arrangement (order, visibility, heights, groups) before the removal.
    pub presentation: Box<WorkspacePresentation>,
    pub suppressed_builtin_gene_bundles: BTreeSet<String>,
}

/// One state-changing user intent.
///
/// Coordinates are 0-based half-open; locus strings use the 1-based inclusive form the locus box
/// shows. Panels and tracks are always named explicitly: the focused panel is resolved by the
/// caller, at the window or protocol boundary, never inside the reducer.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    // ----- Navigation ------------------------------------------------------------------------
    /// Retarget one panel to a locus string.
    GoToLocus { panel_id: PanelId, locus: String },
    /// The locus box / `goto`: one locus retargets `panel_id`; several redefine the workspace
    /// as exactly those loci, one panel each, the first in `panel_id`.
    GoToLoci {
        panel_id: PanelId,
        loci: Vec<String>,
    },
    /// Pan, zoom or fit one panel.
    ChangeViewport {
        panel_id: PanelId,
        operation: AutomationViewportOperation,
        commit: ViewportCommit,
    },
    /// Move one panel to an explicit viewport on its current contig. Pointer gestures compute
    /// the viewport (with its sub-base offset) from pixels before they get here.
    SetViewport {
        panel_id: PanelId,
        viewport: Viewport,
        commit: ViewportCommit,
    },
    /// Move one panel to an explicit viewport that may be on another contig (mate and
    /// supplementary navigation). Recorded in the panel's history.
    RetargetPanel {
        panel_id: PanelId,
        viewport: Viewport,
        contig_name: String,
        contig_length: u64,
    },
    /// Step one panel through its own history.
    NavigateHistory {
        panel_id: PanelId,
        step: AutomationHistoryNavigation,
    },
    /// Return one panel to its home locus.
    NavigateHome { panel_id: PanelId },
    /// Move one panel to a named-contig entry (contig picker, undo). `record` adds a history
    /// entry.
    NavigateToEntry {
        panel_id: PanelId,
        entry: NavigationEntry,
        record: bool,
    },

    // ----- Panel management ------------------------------------------------------------------
    /// Open a new panel at a locus; it becomes the focused panel.
    AddPanel { locus: String },
    /// Close a panel. The last panel cannot be closed.
    ClosePanel { panel_id: PanelId },
    /// Move a panel to a position in the left-to-right order.
    MovePanel { panel_id: PanelId, index: usize },
    /// Give a panel keyboard and locus-box focus.
    ActivatePanel { panel_id: PanelId },
    /// Reopen a closed panel with its identity and place (the inverse of `ClosePanel`).
    RestorePanel(PanelRestore),

    // ----- Track management ------------------------------------------------------------------
    /// Reorder, show, hide, collapse, resize or group tracks.
    ApplyTrackIntents { intents: Vec<TrackIntent> },
    /// Put the whole track arrangement back (the inverse of `ApplyTrackIntents`).
    RestorePresentation {
        presentation: Box<WorkspacePresentation>,
    },
    /// Remove tracks from the workspace and release their engines.
    RemoveTracks { track_ids: Vec<TrackId> },
    /// Reopen removed tracks from their saved configuration (the inverse of `RemoveTracks`).
    RestoreTracks(TrackRestore),

    // ----- Regions of interest and the review queue ------------------------------------------
    /// Replace the regions of interest and the review queue with edited copies. The list
    /// windows edit drafts and commit them whole; the inverse is the lists they replaced.
    SetWorkspaceLists {
        regions: Vec<RegionOfInterest>,
        review_items: Vec<ReviewItem>,
    },
}

impl Action {
    /// The panel the action is addressed to, when it names one.
    pub const fn panel_id(&self) -> Option<PanelId> {
        match self {
            Self::GoToLocus { panel_id, .. }
            | Self::GoToLoci { panel_id, .. }
            | Self::ChangeViewport { panel_id, .. }
            | Self::SetViewport { panel_id, .. }
            | Self::RetargetPanel { panel_id, .. }
            | Self::NavigateHistory { panel_id, .. }
            | Self::NavigateHome { panel_id }
            | Self::NavigateToEntry { panel_id, .. }
            | Self::ClosePanel { panel_id }
            | Self::MovePanel { panel_id, .. }
            | Self::ActivatePanel { panel_id } => Some(*panel_id),
            Self::RestorePanel(restore) => Some(restore.panel_id),
            Self::AddPanel { .. }
            | Self::ApplyTrackIntents { .. }
            | Self::RestorePresentation { .. }
            | Self::RemoveTracks { .. }
            | Self::RestoreTracks(_)
            | Self::SetWorkspaceLists { .. } => None,
        }
    }

    /// A short name for the Edit menu ("Undo close panel").
    pub const fn label(&self) -> &'static str {
        match self {
            Self::GoToLocus { .. }
            | Self::GoToLoci { .. }
            | Self::ChangeViewport { .. }
            | Self::SetViewport { .. }
            | Self::RetargetPanel { .. }
            | Self::NavigateHistory { .. }
            | Self::NavigateHome { .. }
            | Self::NavigateToEntry { .. } => "navigation",
            Self::AddPanel { .. } => "add panel",
            Self::ClosePanel { .. } => "close panel",
            Self::MovePanel { .. } => "move panel",
            Self::ActivatePanel { .. } => "focus panel",
            Self::RestorePanel(_) => "reopen panel",
            Self::ApplyTrackIntents { .. } | Self::RestorePresentation { .. } => "track layout",
            Self::RemoveTracks { .. } => "remove track",
            Self::RestoreTracks(_) => "restore track",
            Self::SetWorkspaceLists { .. } => "region or review edit",
        }
    }
}

/// What applying an action did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionOutcome {
    /// Whether any document state changed.
    pub changed: bool,
    /// The panel the action created or acted on.
    pub panel_id: Option<PanelId>,
    /// The interval a navigation landed on.
    pub interval: Option<GenomicInterval>,
    /// The change set of a track-arrangement action.
    pub track_changes: Option<TrackChangeSet>,
    /// How many tracks a removal took out.
    pub removed_tracks: usize,
}

/// Work the host owes after the reducer changed the panel workspace. Effects carry no data the
/// document does not already hold; they name what must be refreshed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Effect {
    /// A panel's viewport moved.
    ViewportChanged {
        panel_id: PanelId,
        commit: ViewportCommit,
    },
    /// A panel was created and focused.
    PanelAdded {
        panel_id: PanelId,
        previous_focus: PanelId,
    },
    /// A panel left the workspace.
    PanelClosed { panel_id: PanelId },
    /// The left-to-right order changed.
    PanelsReordered,
    /// Focus moved (or was re-asserted on the same panel).
    PanelActivated { previous: PanelId, current: PanelId },
    /// The regions of interest or the review queue were replaced.
    WorkspaceListsChanged {
        regions_changed: bool,
        review_changed: bool,
    },
}
