use crate::inspection::model::{RetainedEvidenceSnapshot, SelectedSupplementaryChain};
use crate::ui::{ScrollbarOverview, WorkspacePaneLayout};
use crate::ui::{TrackFrame, TrackFrameKey};
use crate::workspace::context_lanes::PanelContextScene;
use crate::{HoverInspectorTarget, JunctionArcHover, ui};
use rgv_alignment_scene::{
    AlignmentHit, AlignmentPresentationScene, RowAtlasRevision, RowCoordinateSpaceRevision,
};
use rgv_core::{ContigId, PanelId, PresentationNodeId, TrackId, TrackKind, Viewport};
use rgv_hts::AlignmentRecord;
use rgv_layout::{DisplayList, JunctionArcComponent};
pub(crate) use rgv_session::panels::{
    INITIAL_PANEL_ID, Panel, PanelState, ReadPanelScrollState, ReadRowCoordinateSpace,
    StableRowCameraAnchor,
};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) struct PanelScrollDisplayCache {
    /// CPU-side annotation chrome. Contextual rectangles are projected separately for semantic
    /// hit testing and are rendered exclusively by `ContextSceneGpuRenderer`.
    pub(crate) annotations: DisplayList,
    pub(crate) pinned: DisplayList,
    pub(crate) annotation_overview: ScrollbarOverview,
    pub(crate) annotation_content_bottom: f32,
    pub(crate) annotation_clip_top: f32,
    pub(crate) annotation_clip_bottom: f32,
    pub(crate) supports_fast_scroll: bool,
    pub(crate) read_panels: Vec<ReadPanelDisplayCache>,
}

pub(crate) struct ReadPanelDisplayCache {
    pub(crate) track_id: TrackId,
    /// Target revision of the committed native epoch which supplied unified row authority. Zero
    /// denotes absent or desired-but-not-yet-presented state.
    pub(crate) presentation_target_revision: u64,
    pub(crate) scrollable: DisplayList,
    pub(crate) density_camera: bool,
    /// World-space y coordinate of stable read row zero.
    pub(crate) row_origin: Option<f32>,
    pub(crate) row_height: Option<f32>,
    pub(crate) row_coordinate_authority: ReadRowCoordinateAuthority,
    /// Immutable activity atlas revision which supplied exact vertical extent and navigator
    /// density. `None` is required for Pending/Absent authority.
    pub(crate) row_activity_atlas_revision: Option<RowAtlasRevision>,
    /// Exact normalized committed row intervals represented by the current native display. `None`
    /// means authority is pending/legacy; `Some(empty)` truthfully means no row is materialized.
    pub(crate) materialized_row_ranges: Option<Arc<[(usize, usize)]>>,
    pub(crate) total_rows: Option<usize>,
    /// Optional semantic authority for the read navigator miniature. `Some(empty)` is
    /// deliberately distinct from `None`: a unified scene can truthfully report that the
    /// current genomic camera has no occupied rows; `None` means row authority is unavailable.
    pub(crate) authoritative_overview: Option<ScrollbarOverview>,
    pub(crate) overview: ScrollbarOverview,
    pub(crate) actual_content_bottom: f32,
    pub(crate) clip_top: f32,
    pub(crate) clip_bottom: f32,
    /// Top of the panel's read rows, below its header.
    pub(crate) content_top: f32,
    /// What the panel says instead of, or over, its reads. A banner notice is painted by the UI
    /// over the GPU reads; any other is part of `scrollable`.
    pub(crate) notice: Option<ReadPanelNotice>,
}

/// What a read panel says instead of, or over, its reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReadPanelNotice {
    pub(crate) kind: ReadPanelNoticeKind,
    /// Short text for the panel and its gutter row.
    pub(crate) summary: String,
    /// The full explanation (for a failure, the error chain), shown on hover. Failures are
    /// also reported to the notification log.
    pub(crate) detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadPanelNoticeKind {
    /// Progress or a limit (opening, preparing, too many alignments), drawn in the read stack.
    Status,
    /// Reads cannot be shown here (the contig is not in the file, the cache is too small).
    Unavailable,
    /// Loading failed. `retry` when the read panel can ask for the view again.
    Failure { retry: bool },
}

impl ReadPanelNotice {
    /// Painted by the UI as an opaque banner over the (dimmed) reads rather than drawn into
    /// the read stack, where the GPU reads would paint over it.
    pub(crate) const fn is_banner(&self) -> bool {
        !matches!(self.kind, ReadPanelNoticeKind::Status)
    }

    pub(crate) fn status(summary: impl Into<String>) -> Self {
        Self {
            kind: ReadPanelNoticeKind::Status,
            summary: summary.into(),
            detail: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadPanelMaterializationCoverage {
    Covered {
        materialized: (usize, usize),
        required: (usize, usize),
    },
    PartiallyCovered {
        materialized: (usize, usize),
        required: (usize, usize),
        intersection: (usize, usize),
    },
    Unknown,
    Absent {
        required: (usize, usize),
    },
}

impl ReadPanelMaterializationCoverage {
    pub(crate) const fn is_covered(self) -> bool {
        matches!(self, Self::Covered { .. })
    }
}

pub(crate) fn read_panel_materialization_coverage(
    materialized: Option<&[(usize, usize)]>,
    required: Option<(usize, usize)>,
) -> ReadPanelMaterializationCoverage {
    let Some(required @ (required_start, required_end)) = required else {
        return ReadPanelMaterializationCoverage::Unknown;
    };
    let Some(materialized) = materialized else {
        return ReadPanelMaterializationCoverage::Unknown;
    };
    let materialized_hull = materialized.iter().fold(None, |hull, &(start, end)| {
        Some(hull.map_or((start, end), |(left, right): (usize, usize)| {
            (left.min(start), right.max(end))
        }))
    });
    let mut covered_until = required_start;
    let mut intersection = None;
    for &(start, end) in materialized {
        let overlap_start = start.max(required_start);
        let overlap_end = end.min(required_end);
        if overlap_start < overlap_end {
            intersection = Some(intersection.map_or(
                (overlap_start, overlap_end),
                |(left, right): (usize, usize)| (left.min(overlap_start), right.max(overlap_end)),
            ));
        }
        if start <= covered_until && end > covered_until {
            covered_until = end.min(required_end);
        }
    }
    if covered_until >= required_end {
        return ReadPanelMaterializationCoverage::Covered {
            materialized: materialized_hull.unwrap_or(required),
            required,
        };
    }
    if let Some(intersection) = intersection {
        ReadPanelMaterializationCoverage::PartiallyCovered {
            materialized: materialized_hull.unwrap_or(intersection),
            required,
            intersection,
        }
    } else {
        ReadPanelMaterializationCoverage::Absent { required }
    }
}

/// Canonicalizes the exact row blocks carried by a committed native read scene. Several genomic
/// tiles and geometry families can reference the same row block, so production materialization
/// authority must deduplicate those occurrences and merge only genuinely touching intervals.
pub(crate) fn normalize_materialized_row_ranges(
    ranges: impl IntoIterator<Item = std::ops::Range<u32>>,
) -> Arc<[(usize, usize)]> {
    let mut ranges = ranges
        .into_iter()
        .filter(|range| !range.is_empty())
        .map(|range| (range.start as usize, range.end as usize))
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    let mut normalized = Vec::<(usize, usize)>::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some((_, previous_end)) = normalized.last_mut()
            && start <= *previous_end
        {
            *previous_end = (*previous_end).max(end);
        } else {
            normalized.push((start, end));
        }
    }
    Arc::from(normalized)
}

/// Whether the current display cache can describe the vertical row coordinate space exactly.
///
/// `Pending` is deliberately not an extent authority: it is used while truthful pixels can be
/// shown for the new genomic camera before its exact activity profile arrives. The previous
/// vertical camera and thumb geometry remain visible as a provisional, noninteractive waypoint;
/// exact row consumers continue to reject it until the committed profile arrives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReadRowCoordinateAuthority {
    Absent,
    Pending,
    Exact(ReadRowCoordinateSpace),
}

impl ReadRowCoordinateAuthority {
    #[must_use]
    pub(crate) const fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }

    #[must_use]
    pub(crate) const fn is_exact(self) -> bool {
        matches!(self, Self::Exact(_))
    }

    #[must_use]
    pub(crate) const fn exact(self) -> Option<ReadRowCoordinateSpace> {
        match self {
            Self::Exact(coordinate_space) => Some(coordinate_space),
            Self::Absent | Self::Pending => None,
        }
    }
}

/// Downgrades an old unified cache to provisional authority when native publication moves to a
/// target whose row semantics differ. A vertical camera, clip, or surface-only change may advance
/// the full pixel target while the same genomic row profile remains exact; callers prove that
/// narrower compatibility independently.
pub(crate) const fn current_read_row_authority(
    cached_target_revision: u64,
    current_target_revision: u64,
    committed_row_target_still_exact: bool,
    authority: ReadRowCoordinateAuthority,
) -> ReadRowCoordinateAuthority {
    if cached_target_revision == 0
        || cached_target_revision == current_target_revision
        || committed_row_target_still_exact
    {
        authority
    } else {
        match authority {
            ReadRowCoordinateAuthority::Exact(_) | ReadRowCoordinateAuthority::Pending => {
                ReadRowCoordinateAuthority::Pending
            }
            ReadRowCoordinateAuthority::Absent => ReadRowCoordinateAuthority::Absent,
        }
    }
}

/// Compares one cached scrollbar derivative with the exact row authority committed beside native
/// pixels. Target revision alone is insufficient because an exact worker publication can replace
/// the row atlas/profile without moving the camera.
pub(crate) fn read_row_cache_matches_committed_activity(
    cached_target_revision: u64,
    cached_authority: ReadRowCoordinateAuthority,
    cached_atlas_revision: Option<RowAtlasRevision>,
    cached_total_rows: Option<usize>,
    committed_target_revision: u64,
    committed_activity: Option<(RowCoordinateSpaceRevision, RowAtlasRevision, usize)>,
) -> bool {
    match committed_activity {
        Some((coordinate_space, atlas_revision, active_row_end)) => {
            cached_target_revision == committed_target_revision
                && cached_authority == ReadRowCoordinateAuthority::Exact(coordinate_space)
                && cached_atlas_revision == Some(atlas_revision)
                && cached_total_rows == Some(active_row_end)
        }
        None => {
            cached_target_revision == committed_target_revision
                && cached_authority == ReadRowCoordinateAuthority::Pending
                && cached_atlas_revision.is_none()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TemplateReadEnd {
    First,
    Last,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingReadSelection {
    pub(crate) track_id: TrackId,
    pub(crate) read_name: Vec<u8>,
    pub(crate) target_contig: Option<ContigId>,
    pub(crate) target_start: Option<u64>,
    pub(crate) template_end: Option<TemplateReadEnd>,
    /// Center the resolved record in its read pane without changing its row.
    pub(crate) reveal_when_resolved: bool,
}

impl PendingReadSelection {
    pub(crate) fn by_name(track_id: TrackId, read_name: Vec<u8>) -> Option<Self> {
        (!read_name.is_empty()).then_some(Self {
            track_id,
            read_name,
            target_contig: None,
            target_start: None,
            template_end: None,
            reveal_when_resolved: false,
        })
    }

    pub(crate) fn for_record(track_id: TrackId, record: &AlignmentRecord) -> Option<Self> {
        let mut selection = Self::by_name(track_id, record.read_name.clone())?;
        selection.target_contig = Some(record.contig);
        selection.target_start = Some(record.start);
        selection.template_end = template_read_end(record.flags);
        Some(selection)
    }

    pub(crate) fn for_mate(track_id: TrackId, record: &AlignmentRecord) -> Option<Self> {
        if !record.flags.is_paired() || record.flags.is_mate_unmapped() {
            return None;
        }
        let mut selection = Self::by_name(track_id, record.read_name.clone())?;
        selection.target_contig = record.mate_contig;
        selection.target_start = record.mate_start;
        if selection.target_contig.is_none() || selection.target_start.is_none() {
            return None;
        }
        selection.template_end = match template_read_end(record.flags) {
            Some(TemplateReadEnd::First) => Some(TemplateReadEnd::Last),
            Some(TemplateReadEnd::Last) => Some(TemplateReadEnd::First),
            None => None,
        };
        selection.reveal_when_resolved = true;
        Some(selection)
    }

    pub(crate) fn matching_scene_record(
        &self,
        scene: &AlignmentPresentationScene,
    ) -> Option<(rgv_alignment_scene::AlignmentHandle, AlignmentRecord)> {
        scene
            .handles_for_read_name(&self.read_name)
            .into_iter()
            .find_map(|handle| {
                let record = scene.inspect_retained(handle).ok().flatten()?;
                (self
                    .target_contig
                    .is_none_or(|contig| record.contig == contig)
                    && self.target_start.is_none_or(|start| record.start == start)
                    && self
                        .template_end
                        .is_none_or(|read_end| template_read_end(record.flags) == Some(read_end)))
                .then_some((handle, record))
            })
    }

    /// Keeps the exact semantic identity after binding it to one query-local
    /// record index. Reveal is a one-shot navigation request; identity is not.
    pub(crate) fn retained_after_resolution(
        mut self,
        record: Option<&AlignmentRecord>,
    ) -> (Self, bool) {
        let reveal = self.reveal_when_resolved;
        if let Some(record) = record
            && let Some(exact) = Self::for_record(self.track_id, record)
        {
            self = exact;
        }
        self.reveal_when_resolved = false;
        (self, reveal)
    }
}

fn template_read_end(flags: rgv_hts::AlignmentFlags) -> Option<TemplateReadEnd> {
    match (flags.is_first_in_template(), flags.is_last_in_template()) {
        (true, false) => Some(TemplateReadEnd::First),
        (false, true) => Some(TemplateReadEnd::Last),
        _ => None,
    }
}

/// Runtime state owned by one genomic view panel.
///
/// Identity, camera, history, query generation, retained data, interaction
/// throttling, and semantic selection stay together so visible panels cannot
/// suppress or replace one another's state.
pub(crate) struct ViewPanelController {
    /// The device-free half: identity, camera, history and query bookkeeping. Reached through
    /// `Deref`, so `panel.viewport` reads the same as before the split.
    pub(crate) state: PanelState,
    pub(crate) selected_alignment_track: Option<TrackId>,
    pub(crate) selected_alignment_tracks: Vec<TrackId>,
    pub(crate) alignment_selection_anchor: Option<TrackId>,
    /// Alignment read panels selected from the lower read-data section. This
    /// is separate from annotation selection even though both use the same IDs.
    pub(crate) selected_read_tracks: Vec<TrackId>,
    pub(crate) read_selection_anchor: Option<TrackId>,
    pub(crate) selected_pileup_position: Option<u64>,
    pub(crate) selected_modification_position: Option<u64>,
    pub(crate) selected_junction: Option<(u64, u64)>,
    pub(crate) selected_junction_component: Option<JunctionArcComponent>,
    /// Sashimi splice-site filter: only junctions of this track using this site are drawn.
    pub(crate) sashimi_site_filter: Option<(TrackId, rgv_model::SpliceSite)>,
    /// Sashimi transcript filter: only junctions matching the transcript's introns are drawn.
    pub(crate) sashimi_intron_filter:
        Option<crate::workspace::alignment_management::sashimi_presentation::SashimiIntronFilter>,
    pub(crate) selected_chain: Option<SelectedSupplementaryChain>,
    pub(crate) retained_evidence: Option<RetainedEvidenceSnapshot>,
    /// Durable semantic identity used to resolve a read after cross-locus navigation.
    pub(crate) pending_read_selection: Option<PendingReadSelection>,
    /// Stable-handle reveal request for the unified scene path. Query-local record indexes never
    /// cross this boundary.
    pub(crate) pending_alignment_reveal: Option<(TrackId, rgv_alignment_scene::AlignmentHandle)>,
    /// Stable scene hit captured from the same immutable panel frame used for rendering.
    pub(crate) hovered_alignment_hit: Option<AlignmentHit>,
    pub(crate) selected_alignment_hit: Option<AlignmentHit>,
    pub(crate) hover_inspector_target: Option<HoverInspectorTarget>,
    pub(crate) hovered_junction_arc: Option<JunctionArcHover>,
    pub(crate) display_list: DisplayList,
    /// Rectangle subset owned by the generic renderer. Contextual lane rectangles remain in
    /// `display_list` for picking/export but must never be uploaded through this path.
    pub(crate) generic_display_list: DisplayList,
    /// Immutable contextual-lane leases and their current CPU reference projection. Alignment
    /// pixels live in the unified alignment scene; this scene owns only reference, genes, and
    /// auxiliary annotations.
    pub(crate) context_scene: PanelContextScene,
    pub(crate) scroll_display_cache: Option<PanelScrollDisplayCache>,
    pub(crate) track_frames: Vec<TrackFrame>,
    pub(crate) pane_layout: WorkspacePaneLayout,
    pub(crate) selected_track: Option<TrackFrameKey>,
    /// Canonical pane-qualified selection. Legacy per-channel TrackId vectors
    /// remain projections used by alignment settings and evidence code.
    pub(crate) selected_presentation_nodes: Vec<PresentationNodeId>,
    pub(crate) presentation_selection_anchor: Option<PresentationNodeId>,
}

pub(crate) fn composite_panel_display_lists(panels: &PanelWorkspace) -> DisplayList {
    let mut composite = DisplayList::default();
    for panel in panels.iter() {
        if panel.id == panels.active().id {
            composite
                .rectangles
                .extend(panel.display_list.rectangles.iter().copied());
        } else {
            composite
                .rectangles
                .extend(
                    panel
                        .display_list
                        .rectangles
                        .iter()
                        .copied()
                        .map(|mut rectangle| {
                            // GPU hover and selection IDs are local to a panel.
                            // Inactive panels keep their semantic display lists for
                            // picking after activation, but cannot mirror an active
                            // panel's highlight accidentally.
                            rectangle.object_id = 0;
                            rectangle
                        }),
                );
        }
        composite
            .texts
            .extend(panel.display_list.texts.iter().cloned());
    }
    composite
}

/// GPU input for the generic rectangle path. Contextual and alignment scene rectangles are
/// deliberately absent even though they remain in each panel's semantic `display_list`.
pub(crate) fn composite_panel_generic_display_lists(panels: &PanelWorkspace) -> DisplayList {
    let mut composite = DisplayList::default();
    for panel in panels.iter() {
        composite
            .rectangles
            .extend(panel.generic_display_list.rectangles.iter().copied());
        composite
            .texts
            .extend(panel.generic_display_list.texts.iter().cloned());
    }
    composite
}

pub(crate) fn maximum_annotation_frame_heights(
    panels: &PanelWorkspace,
) -> BTreeMap<PresentationNodeId, f32> {
    let mut heights = BTreeMap::new();
    for item in panels
        .iter()
        .flat_map(|panel| panel.pane_layout.annotation.items.iter())
    {
        let height = (item.frame.bottom - item.frame.top).max(0.0);
        heights
            .entry(item.id)
            .and_modify(|maximum: &mut f32| *maximum = maximum.max(height))
            .or_insert(height);
    }
    heights
}

fn read_panel_allocation_rect(
    layout: &WorkspacePaneLayout,
    track_id: TrackId,
) -> Option<egui::Rect> {
    layout
        .reads
        .items
        .iter()
        .filter_map(read_panel_allocation)
        .find_map(|(candidate, rect)| (candidate == track_id).then_some(rect))
}

fn read_panel_allocation_at_y(layout: &WorkspacePaneLayout, screen_y: f32) -> Option<TrackId> {
    layout
        .reads
        .items
        .iter()
        .filter_map(read_panel_allocation)
        .find_map(|(track_id, rect)| {
            (screen_y >= rect.top() && screen_y < rect.bottom()).then_some(track_id)
        })
}

fn read_panel_allocation(item: &ui::PaneItemLayout) -> Option<(TrackId, egui::Rect)> {
    let PresentationNodeId::Track {
        channel: ui::TrackPane::Reads,
        track_id,
    } = item.id
    else {
        return None;
    };
    (item.frame.kind == TrackKind::Alignment
        && item.effective_content_visible
        && item.row_rect.height() > 0.0)
        .then_some((track_id, item.row_rect))
}

impl std::ops::Deref for ViewPanelController {
    type Target = PanelState;

    fn deref(&self) -> &PanelState {
        &self.state
    }
}

impl std::ops::DerefMut for ViewPanelController {
    fn deref_mut(&mut self) -> &mut PanelState {
        &mut self.state
    }
}

impl Panel for ViewPanelController {
    fn state(&self) -> &PanelState {
        &self.state
    }

    fn state_mut(&mut self) -> &mut PanelState {
        &mut self.state
    }

    fn selected_alignment_track(&self) -> Option<TrackId> {
        self.selected_alignment_track
    }

    fn selected_pileup_position(&self) -> Option<u64> {
        self.selected_pileup_position
    }

    fn selected_modification_position(&self) -> Option<u64> {
        self.selected_modification_position
    }

    fn clear_pending_alignment_reveal(&mut self, only_track: Option<TrackId>) {
        if self.pending_alignment_reveal.is_some_and(|(pending_track, _)| {
            only_track.is_none_or(|track_id| pending_track == track_id)
        }) {
            self.pending_alignment_reveal = None;
        }
    }

    fn from_state(state: PanelState) -> Self {
        Self {
            state,
            selected_alignment_track: None,
            selected_alignment_tracks: Vec::new(),
            alignment_selection_anchor: None,
            selected_read_tracks: Vec::new(),
            read_selection_anchor: None,
            selected_pileup_position: None,
            selected_modification_position: None,
            selected_junction: None,
            selected_junction_component: None,
            sashimi_site_filter: None,
            sashimi_intron_filter: None,
            selected_chain: None,
            retained_evidence: None,
            pending_read_selection: None,
            pending_alignment_reveal: None,
            hovered_alignment_hit: None,
            selected_alignment_hit: None,
            hover_inspector_target: None,
            hovered_junction_arc: None,
            display_list: DisplayList::default(),
            generic_display_list: DisplayList::default(),
            context_scene: PanelContextScene::default(),
            scroll_display_cache: None,
            track_frames: Vec::new(),
            pane_layout: WorkspacePaneLayout::default(),
            selected_track: None,
            selected_presentation_nodes: Vec::new(),
            presentation_selection_anchor: None,
        }
    }
}

/// The desktop's panel collection: the session's ordered panel set over panels that also carry
/// display caches and layout.
pub(crate) type PanelWorkspace = rgv_session::panels::PanelWorkspace<ViewPanelController>;

/// How far the shared annotation camera may scroll: the smallest overflow of any panel.
pub(crate) fn maximum_annotation_scroll_points(panels: &PanelWorkspace) -> f32 {
    if panels
        .panels
        .iter()
        .any(|panel| panel.scroll_display_cache.is_none())
    {
        return 0.0;
    }
    panels
        .panels
        .iter()
        .filter_map(|panel| panel.scroll_display_cache.as_ref())
        .map(|cache| (cache.annotation_content_bottom - cache.annotation_clip_bottom).max(0.0))
        .reduce(f32::min)
        .unwrap_or(0.0)
}

impl ViewPanelController {
    pub(crate) fn new(
        id: PanelId,
        viewport: Viewport,
        requested_locus: Option<String>,
        contig_name: String,
        contig_length: u64,
    ) -> Self {
        Self::from_state(PanelState::new(
            id,
            viewport,
            requested_locus,
            contig_name,
            contig_length,
        ))
    }

    /// Sets (`Some`) or clears (`None`) the Sashimi splice-site filter of one track's lane.
    /// Returns whether it changed. Hover is dropped: the arc under the pointer may be gone.
    pub(crate) fn set_sashimi_site_filter(
        &mut self,
        track_id: TrackId,
        site: Option<rgv_model::SpliceSite>,
    ) -> bool {
        let next = site.map(|site| (track_id, site));
        if self.sashimi_site_filter == next {
            return false;
        }
        self.sashimi_site_filter = next;
        self.hovered_junction_arc = None;
        true
    }

    /// Clears the Sashimi splice-site and transcript filters. Returns whether one was set.
    pub(crate) fn clear_sashimi_filters(&mut self) -> bool {
        if self.sashimi_site_filter.is_none() && self.sashimi_intron_filter.is_none() {
            return false;
        }
        self.sashimi_site_filter = None;
        self.sashimi_intron_filter = None;
        self.hovered_junction_arc = None;
        true
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected_track = None;
        self.selected_presentation_nodes.clear();
        self.presentation_selection_anchor = None;
        self.selected_alignment_track = None;
        self.selected_alignment_tracks.clear();
        self.alignment_selection_anchor = None;
        self.selected_read_tracks.clear();
        self.read_selection_anchor = None;
        self.selected_pileup_position = None;
        self.selected_modification_position = None;
        self.selected_junction = None;
        self.selected_junction_component = None;
        self.selected_feature = None;
        self.selected_variant = None;
        self.selected_gwas = None;
        self.selected_quantitative = None;
        self.selected_copy_number = None;
        self.selected_mutation = None;
        self.selected_interaction = None;
        self.selected_chain = None;
        self.retained_evidence = None;
        self.pending_read_selection = None;
        self.pending_alignment_reveal = None;
        self.hovered_alignment_hit = None;
        self.selected_alignment_hit = None;
        self.hover_inspector_target = None;
        self.hovered_junction_arc = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::GenomicInterval;

    fn live_read_layout(track_id: TrackId, row_rect: egui::Rect) -> WorkspacePaneLayout {
        let frame = TrackFrame {
            key: TrackFrameKey::Alignment(track_id),
            channel: ui::TrackPane::Reads,
            name: "Reads".to_owned(),
            kind: TrackKind::Alignment,
            source_label: None,
            top: row_rect.top(),
            bottom: row_rect.bottom(),
            visible: true,
            collapsed: false,
            frozen_bottom: Some(row_rect.top() + ui::TRACK_HEADER_HEIGHT),
            subtracks: Vec::new(),
        };
        WorkspacePaneLayout {
            annotation: ui::PaneLayoutSnapshot::empty(ui::TrackPane::Annotation),
            reads: ui::PaneLayoutSnapshot {
                channel: ui::TrackPane::Reads,
                pane_rect: row_rect,
                content_bottom: row_rect.bottom(),
                scroll_points: 0.0,
                stack_scroll_points: 0.0,
                items: vec![ui::PaneItemLayout {
                    id: PresentationNodeId::Track {
                        channel: ui::TrackPane::Reads,
                        track_id,
                    },
                    frame,
                    row_rect,
                    header_rect: egui::Rect::from_min_max(
                        row_rect.min,
                        egui::pos2(row_rect.right(), row_rect.top() + ui::TRACK_HEADER_HEIGHT),
                    ),
                    content_rect: Some(row_rect),
                    clip_rect: row_rect,
                    desired_content_visible: true,
                    effective_content_visible: true,
                }],
            },
        }
    }

    #[test]
    fn read_scrollbar_targeting_uses_the_live_panel_allocation() {
        let track_id = TrackId::new(73);
        let allocation = egui::Rect::from_min_max(egui::pos2(0.0, 210.0), egui::pos2(800.0, 570.0));
        let layout = live_read_layout(track_id, allocation);

        assert_eq!(
            read_panel_allocation_rect(&layout, track_id),
            Some(allocation)
        );
        assert_eq!(read_panel_allocation_at_y(&layout, 569.0), Some(track_id));
        assert_eq!(read_panel_allocation_at_y(&layout, 570.0), None);

        // A scene cache from the previous frame could still end at 420. The live allocation is
        // intentionally the only input here, so the lower part of the current panel stays a
        // valid scrollbar target while that cache catches up.
        assert_eq!(read_panel_allocation_at_y(&layout, 500.0), Some(track_id));
    }

    #[test]
    fn sashimi_site_filter_is_set_once_and_cleared_with_the_transcript_filter() {
        let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
        let mut panel = ViewPanelController::new(
            PanelId::new(1),
            Viewport::new(interval, 800.0).unwrap(),
            None,
            "chr3".to_owned(),
            10_000,
        );
        let track = TrackId::new(4);
        let site = rgv_model::SpliceSite::end_of(149_371_868);
        assert!(!panel.clear_sashimi_filters());
        assert!(panel.set_sashimi_site_filter(track, Some(site)));
        assert!(!panel.set_sashimi_site_filter(track, Some(site)));
        assert_eq!(panel.sashimi_site_filter, Some((track, site)));
        panel.sashimi_intron_filter = Some(
            crate::workspace::alignment_management::sashimi_presentation::SashimiIntronFilter {
                track_id: track,
                introns: std::sync::Arc::new(std::collections::BTreeSet::from([(10, 20)])),
                label: "ENST1".to_owned(),
            },
        );
        assert!(panel.clear_sashimi_filters());
        assert_eq!(panel.sashimi_site_filter, None);
        assert!(panel.sashimi_intron_filter.is_none());
        assert!(!panel.clear_sashimi_filters());
    }

    #[test]
    fn mate_panel_read_camera_is_independent_from_the_source_panel() {
        let track_id = TrackId::new(81);
        let interval = GenomicInterval::new(ContigId::new(0), 100, 200).unwrap();
        let viewport = Viewport::new(interval, 800.0).unwrap();
        let mut source = ViewPanelController::new(
            PanelId::new(1),
            viewport,
            Some("chr1:101-200".to_owned()),
            "chr1".to_owned(),
            10_000,
        );
        let mut mate = ViewPanelController::new(
            PanelId::new(2),
            viewport,
            Some("chr1:101-200".to_owned()),
            "chr1".to_owned(),
            10_000,
        );
        source.read_panel_scrolls.insert(
            track_id,
            ReadPanelScrollState {
                scroll_points: 420.0,
                content_origin_points: 20.0,
                latent_scroll_fraction: Some(0.4),
                content_bottom_points: 1_600.0,
                density: Some(2.0),
                stable_row_anchor: None,
                scene_contig: Some(ContigId::new(0)),
            },
        );
        mate.read_panel_scrolls.insert(
            track_id,
            ReadPanelScrollState {
                scroll_points: 35.0,
                content_origin_points: 5.0,
                latent_scroll_fraction: Some(0.1),
                content_bottom_points: 340.0,
                density: Some(1.0),
                stable_row_anchor: None,
                scene_contig: Some(ContigId::new(0)),
            },
        );

        let mut workspace = PanelWorkspace::from_ordered(vec![source, mate], PanelId::new(2))
            .expect("two independent panels are valid");
        workspace
            .active_mut()
            .read_panel_scrolls
            .get_mut(&track_id)
            .unwrap()
            .scroll_points = 210.0;

        assert_eq!(
            workspace.panels[0]
                .read_panel_scroll_state(track_id)
                .scroll_points,
            420.0
        );
        assert_eq!(
            workspace.panels[1]
                .read_panel_scroll_state(track_id)
                .scroll_points,
            210.0
        );
        assert_ne!(
            workspace.panels[0]
                .read_panel_scroll_state(track_id)
                .content_bottom_points,
            workspace.panels[1]
                .read_panel_scroll_state(track_id)
                .content_bottom_points
        );
    }

    #[test]
    fn materialization_coverage_never_promotes_missing_authority_to_covered() {
        assert_eq!(
            read_panel_materialization_coverage(None, Some((10, 20))),
            ReadPanelMaterializationCoverage::Unknown
        );
        assert_eq!(
            read_panel_materialization_coverage(Some(&[(0, 100)]), None),
            ReadPanelMaterializationCoverage::Unknown
        );
        assert!(
            read_panel_materialization_coverage(Some(&[(0, 100)]), Some((10, 20))).is_covered()
        );
        assert_eq!(
            read_panel_materialization_coverage(Some(&[(0, 15)]), Some((10, 20))),
            ReadPanelMaterializationCoverage::PartiallyCovered {
                materialized: (0, 15),
                required: (10, 20),
                intersection: (10, 15),
            }
        );
        assert_eq!(
            read_panel_materialization_coverage(Some(&[(0, 5)]), Some((10, 20))),
            ReadPanelMaterializationCoverage::Absent { required: (10, 20) }
        );
        assert!(
            read_panel_materialization_coverage(
                Some(&[(0, 12), (12, 18), (18, 30)]),
                Some((10, 20)),
            )
            .is_covered()
        );
        assert!(matches!(
            read_panel_materialization_coverage(Some(&[(0, 12), (18, 30)]), Some((10, 20)),),
            ReadPanelMaterializationCoverage::PartiallyCovered { .. }
        ));
    }

    #[test]
    fn committed_materialization_normalizes_duplicate_and_disjoint_row_blocks() {
        let normalized =
            normalize_materialized_row_ranges([16..24, 0..8, 0..8, 8..16, 40..48, 44..52, 60..60]);
        assert_eq!(normalized.as_ref(), &[(0, 24), (40, 52)]);
    }
}

impl ViewPanelController {
    pub(crate) fn effective_vertical_scroll_points(&self) -> f32 {
        (self.vertical_scroll_points - self.vertical_content_origin_points).max(0.0)
    }

    /// Returns the physical row allocated to an exposed read panel by the live pane layout.
    ///
    /// Scene display caches are allowed to lag this layout during navigation and window reflow,
    /// so their clip rectangles must never define scrollbar paint or interaction geometry.
    pub(crate) fn live_read_panel_rect(&self, track_id: TrackId) -> Option<egui::Rect> {
        read_panel_allocation_rect(&self.pane_layout, track_id)
    }

    /// Resolves a read panel from the same live allocation used by its scrollbar rail.
    pub(crate) fn live_read_panel_at_y(&self, screen_y: f32) -> Option<TrackId> {
        read_panel_allocation_at_y(&self.pane_layout, screen_y)
    }

    pub(crate) fn live_read_panel_allocations(
        &self,
    ) -> impl Iterator<Item = (TrackId, egui::Rect)> + '_ {
        self.pane_layout
            .reads
            .items
            .iter()
            .filter_map(read_panel_allocation)
    }

    pub(crate) fn read_panel_scroll_state(&self, track_id: TrackId) -> ReadPanelScrollState {
        self.read_panel_scrolls
            .get(&track_id)
            .copied()
            .unwrap_or_default()
    }
}
