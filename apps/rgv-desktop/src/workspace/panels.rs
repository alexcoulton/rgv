use super::super::*;
use crate::ui::{ScrollbarOverview, WorkspacePaneLayout};

#[derive(Default)]
pub(crate) struct PendingQueries {
    pub(crate) alignment: bool,
    pub(crate) reference: bool,
    pub(crate) tracks: bool,
}

pub(crate) struct PanelScrollDisplayCache {
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
    pub(crate) scrollable: DisplayList,
    pub(crate) density_camera: bool,
    pub(crate) row_height: Option<f32>,
    pub(crate) row_layout_revision: Option<u64>,
    pub(crate) overview: ScrollbarOverview,
    pub(crate) actual_content_bottom: f32,
    pub(crate) clip_top: f32,
    pub(crate) clip_bottom: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StableRowCameraAnchor {
    pub(crate) row: usize,
    pub(crate) offset_fraction: f32,
    pub(crate) layout_revision: u64,
    pub(crate) row_height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ReadPanelScrollState {
    pub(crate) scroll_points: f32,
    pub(crate) content_origin_points: f32,
    pub(crate) latent_scroll_fraction: Option<f32>,
    pub(crate) content_bottom_points: f32,
    /// Density selected through this workspace's read navigator. `None` uses
    /// the track's configured default density.
    pub(crate) density: Option<f32>,
    /// Stable row at the top of the viewport plus the fractional offset into
    /// it. This survives changes in total stack height and row density.
    pub(crate) stable_row_anchor: Option<StableRowCameraAnchor>,
}

impl ReadPanelScrollState {
    pub(crate) fn effective_scroll_points(self) -> f32 {
        (self.scroll_points - self.content_origin_points).max(0.0)
    }
}

/// Runtime state owned by one genomic view panel.
///
/// Identity, camera, history, query generation, retained data, interaction
/// throttling, and semantic selection stay together so visible panels cannot
/// suppress or replace one another's state.
pub(crate) struct ViewPanelController {
    pub(crate) id: PanelId,
    pub(crate) viewport: Viewport,
    pub(crate) generation_clock: GenerationClock,
    pub(crate) navigation_history: NavigationHistory,
    pub(crate) home_interval: GenomicInterval,
    pub(crate) requested_locus: Option<String>,
    pub(crate) contig_name: String,
    pub(crate) contig_length: u64,
    pub(crate) selected_record: Option<usize>,
    pub(crate) selected_alignment_track: Option<TrackId>,
    pub(crate) selected_alignment_tracks: Vec<TrackId>,
    pub(crate) alignment_selection_anchor: Option<TrackId>,
    /// Alignment read panels selected from the lower read-data section. This
    /// is separate from annotation selection even though both use the same IDs.
    pub(crate) selected_read_tracks: Vec<TrackId>,
    pub(crate) read_selection_anchor: Option<TrackId>,
    pub(crate) selected_read_event: Option<ReadEventSelection>,
    pub(crate) selected_pileup_position: Option<u64>,
    pub(crate) selected_modification_position: Option<u64>,
    pub(crate) selected_junction: Option<(u64, u64)>,
    pub(crate) selected_feature: Option<FeatureSelection>,
    pub(crate) selected_variant: Option<VariantSelection>,
    pub(crate) selected_gwas: Option<GwasSelection>,
    pub(crate) selected_quantitative: Option<QuantitativeSelection>,
    pub(crate) selected_copy_number: Option<CopyNumberSelection>,
    pub(crate) selected_mutation: Option<MutationSelection>,
    pub(crate) selected_interaction: Option<InteractionSelection>,
    pub(crate) selected_chain: Option<SelectedSupplementaryChain>,
    pub(crate) retained_evidence: Option<RetainedEvidenceSnapshot>,
    pub(crate) pending_read_selection: Option<(TrackId, Vec<u8>)>,
    pub(crate) hovered_record: Option<(TrackId, usize)>,
    pub(crate) hover_inspector_target: Option<HoverInspectorTarget>,
    pub(crate) display_list: DisplayList,
    /// Non-evidentiary read geometry derived from a complete canonical horizon.
    /// It is rendered above the retained texture with zero object IDs only.
    pub(crate) navigation_provisional_display: Option<DisplayList>,
    pub(crate) scroll_display_cache: Option<PanelScrollDisplayCache>,
    pub(crate) reference_slice: Option<Arc<ReferenceSlice>>,
    pub(crate) reference_settled_generation: Option<u64>,
    pub(crate) pending_queries: PendingQueries,
    pub(crate) track_snapshots: Vec<TrackSnapshot>,
    pub(crate) track_snapshot_interval: Option<GenomicInterval>,
    pub(crate) track_settled_generation: Option<u64>,
    pub(crate) vertical_scroll_points: f32,
    /// Position of the laid-out scrollable content inside the density-expanded
    /// vertical world. Rendering uses `vertical_scroll_points - origin`, while
    /// the scrollbar and overview use the real world-space camera position.
    pub(crate) vertical_content_origin_points: f32,
    /// Conventional parent scroll for an overflowing stack of read panels.
    /// Per-panel read/data scroll remains in `read_panel_scrolls`.
    pub(crate) read_stack_scroll_points: f32,
    pub(crate) read_stack_content_bottom_points: f32,
    /// Normalized vertical camera retained while the content has no scroll
    /// range, so a density gesture does not snap its thumb back to the top.
    pub(crate) latent_vertical_scroll_fraction: Option<f32>,
    /// Independent vertical cameras for the read panel belonging to each
    /// alignment track. The legacy scalar fields above mirror the primary
    /// track for backward-compatible sessions and automation.
    pub(crate) read_panel_scrolls: BTreeMap<TrackId, ReadPanelScrollState>,
    pub(crate) content_bottom_points: f32,
    pub(crate) track_frames: Vec<TrackFrame>,
    pub(crate) pane_layout: WorkspacePaneLayout,
    pub(crate) selected_track: Option<TrackFrameKey>,
    /// Canonical pane-qualified selection. Legacy per-channel TrackId vectors
    /// remain projections used by alignment settings and evidence code.
    pub(crate) selected_presentation_nodes: Vec<PresentationNodeId>,
    pub(crate) presentation_selection_anchor: Option<PresentationNodeId>,
    pub(crate) navigation_presentation: NavigationPresentation,
    pub(crate) navigation_query_schedule: NavigationQuerySchedule,
    pub(crate) reference_query_error_generation: Option<u64>,
    pub(crate) track_query_error_generation: Option<u64>,
}

pub(crate) const INITIAL_PANEL_ID: PanelId = PanelId::new(1);

/// Ordered runtime panel collection.
///
/// Active-panel access is deliberately explicit. This keeps code which acts on
/// one viewport visually distinct from code which iterates over the workspace.
pub(crate) struct PanelWorkspace {
    pub(crate) panels: Vec<ViewPanelController>,
    pub(crate) active: usize,
    /// One annotation camera is shared by every genomic panel. The left gutter
    /// describes workspace rows, so a per-panel offset would make that gutter
    /// truthful for at most one locus at a time.
    pub(crate) annotation_scroll_points: f32,
}

impl PanelWorkspace {
    pub(crate) fn new(initial: ViewPanelController) -> Self {
        Self {
            panels: vec![initial],
            active: 0,
            annotation_scroll_points: 0.0,
        }
    }

    pub(crate) fn from_ordered(
        panels: Vec<ViewPanelController>,
        active_id: PanelId,
    ) -> Option<Self> {
        if panels.is_empty()
            || panels.len() > MAXIMUM_VIEW_PANELS
            || panels
                .iter()
                .map(|panel| panel.id)
                .collect::<std::collections::HashSet<_>>()
                .len()
                != panels.len()
        {
            return None;
        }
        let active = panels.iter().position(|panel| panel.id == active_id)?;
        Some(Self {
            panels,
            active,
            annotation_scroll_points: 0.0,
        })
    }

    pub(crate) fn from_session(
        session: &SessionDocument,
        canvas_width: f32,
        content_top: f32,
    ) -> Result<Self> {
        let panel_width = f64::from(canvas_width.max(1.0));
        let panels = session
            .panels
            .iter()
            .map(|saved| {
                let contig_length = saved
                    .locus
                    .end
                    .saturating_mul(2)
                    .max(saved.locus.end.saturating_add(1_000_000));
                let interval =
                    GenomicInterval::new(ContigId::new(0), saved.locus.start, saved.locus.end)?;
                let viewport = Viewport::new(interval, panel_width)?;
                let locus = format!(
                    "{}:{}-{}",
                    saved.locus.contig,
                    saved.locus.start.saturating_add(1),
                    saved.locus.end
                );
                let mut panel = ViewPanelController::new(
                    saved.id,
                    viewport,
                    Some(locus),
                    saved.locus.contig.clone(),
                    contig_length,
                );
                panel.vertical_scroll_points = saved.vertical_scroll_points;
                panel.vertical_content_origin_points = saved.vertical_content_origin_points;
                panel.read_stack_scroll_points = saved.read_stack_scroll_points;
                panel.read_panel_scrolls = saved
                    .read_panel_states
                    .iter()
                    .map(|state| {
                        (
                            state.track_id,
                            ReadPanelScrollState {
                                scroll_points: state.scroll_points,
                                content_origin_points: state.content_origin_points,
                                latent_scroll_fraction: state.latent_scroll_fraction,
                                content_bottom_points: state.content_bottom_points,
                                density: state.density,
                                stable_row_anchor: None,
                            },
                        )
                    })
                    .collect();
                panel.content_bottom_points = content_top;
                Ok(panel)
            })
            .collect::<Result<Vec<_>>>()?;
        let annotation_scroll_points = session
            .panels
            .iter()
            .find(|panel| panel.id == session.active_panel)
            .map_or(0.0, |panel| panel.annotation_scroll_points);
        let mut workspace = Self::from_ordered(panels, session.active_panel)
            .context("session active view panel is unavailable")?;
        // Older sessions could persist different annotation offsets per locus.
        // The active locus is the least surprising authority when migrating to
        // the workspace-wide annotation camera.
        workspace.annotation_scroll_points = annotation_scroll_points;
        Ok(workspace)
    }

    pub(crate) fn len(&self) -> usize {
        self.panels.len()
    }

    pub(crate) fn active(&self) -> &ViewPanelController {
        &self.panels[self.active]
    }

    pub(crate) fn active_mut(&mut self) -> &mut ViewPanelController {
        &mut self.panels[self.active]
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = &ViewPanelController> {
        self.panels.iter()
    }

    pub(crate) fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = &mut ViewPanelController> {
        self.panels.iter_mut()
    }

    pub(crate) fn get(&self, id: PanelId) -> Option<&ViewPanelController> {
        self.panels.iter().find(|panel| panel.id == id)
    }

    pub(crate) fn get_mut(&mut self, id: PanelId) -> Option<&mut ViewPanelController> {
        self.panels.iter_mut().find(|panel| panel.id == id)
    }

    pub(crate) fn maximum_annotation_scroll_points(&self) -> f32 {
        if self
            .panels
            .iter()
            .any(|panel| panel.scroll_display_cache.is_none())
        {
            return 0.0;
        }
        self.panels
            .iter()
            .filter_map(|panel| panel.scroll_display_cache.as_ref())
            .map(|cache| (cache.annotation_content_bottom - cache.annotation_clip_bottom).max(0.0))
            .reduce(f32::min)
            .unwrap_or(0.0)
    }

    pub(crate) fn next_id(&self) -> PanelId {
        PanelId::new(
            self.panels
                .iter()
                .map(|panel| panel.id.get())
                .max()
                .unwrap_or_default()
                .saturating_add(1),
        )
    }

    pub(crate) fn push(&mut self, panel: ViewPanelController) -> bool {
        if self.panels.len() >= MAXIMUM_VIEW_PANELS || self.get(panel.id).is_some() {
            return false;
        }
        self.panels.push(panel);
        true
    }

    pub(crate) fn activate(&mut self, id: PanelId) -> bool {
        let Some(index) = self.panels.iter().position(|panel| panel.id == id) else {
            return false;
        };
        self.active = index;
        true
    }

    pub(crate) fn move_panel(&mut self, id: PanelId, new_index: usize) -> bool {
        let Some(old_index) = self.panels.iter().position(|panel| panel.id == id) else {
            return false;
        };
        let active_id = self.panels[self.active].id;
        let panel = self.panels.remove(old_index);
        self.panels.insert(new_index.min(self.panels.len()), panel);
        self.active = self
            .panels
            .iter()
            .position(|panel| panel.id == active_id)
            .expect("active panel survives a reorder");
        true
    }

    pub(crate) fn remove(&mut self, id: PanelId) -> Option<ViewPanelController> {
        if self.panels.len() == 1 {
            return None;
        }
        let index = self.panels.iter().position(|panel| panel.id == id)?;
        let removed_was_active = index == self.active;
        let panel = self.panels.remove(index);
        if removed_was_active {
            self.active = index.min(self.panels.len().saturating_sub(1));
        } else if index < self.active {
            self.active -= 1;
        }
        Some(panel)
    }
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

impl ViewPanelController {
    pub(crate) fn new(
        id: PanelId,
        viewport: Viewport,
        requested_locus: Option<String>,
        contig_name: String,
        contig_length: u64,
    ) -> Self {
        let initial_interval = viewport.interval();
        Self {
            id,
            viewport,
            generation_clock: GenerationClock::default(),
            navigation_history: NavigationHistory::new(initial_interval),
            home_interval: initial_interval,
            requested_locus,
            contig_name,
            contig_length,
            selected_record: None,
            selected_alignment_track: None,
            selected_alignment_tracks: Vec::new(),
            alignment_selection_anchor: None,
            selected_read_tracks: Vec::new(),
            read_selection_anchor: None,
            selected_read_event: None,
            selected_pileup_position: None,
            selected_modification_position: None,
            selected_junction: None,
            selected_feature: None,
            selected_variant: None,
            selected_gwas: None,
            selected_quantitative: None,
            selected_copy_number: None,
            selected_mutation: None,
            selected_interaction: None,
            selected_chain: None,
            retained_evidence: None,
            pending_read_selection: None,
            hovered_record: None,
            hover_inspector_target: None,
            display_list: DisplayList::default(),
            navigation_provisional_display: None,
            scroll_display_cache: None,
            reference_slice: None,
            reference_settled_generation: None,
            pending_queries: PendingQueries::default(),
            track_snapshots: Vec::new(),
            track_snapshot_interval: None,
            track_settled_generation: None,
            vertical_scroll_points: 0.0,
            vertical_content_origin_points: 0.0,
            read_stack_scroll_points: 0.0,
            read_stack_content_bottom_points: 0.0,
            latent_vertical_scroll_fraction: None,
            read_panel_scrolls: BTreeMap::new(),
            content_bottom_points: 0.0,
            track_frames: Vec::new(),
            pane_layout: WorkspacePaneLayout::default(),
            selected_track: None,
            selected_presentation_nodes: Vec::new(),
            presentation_selection_anchor: None,
            navigation_presentation: NavigationPresentation::initial(
                initial_interval,
                0,
                Instant::now(),
            ),
            navigation_query_schedule: NavigationQuerySchedule::default(),
            reference_query_error_generation: None,
            track_query_error_generation: None,
        }
    }

    pub(crate) fn request(&self) -> PanelGeneration {
        PanelGeneration::new(self.id, self.generation_clock.current())
    }

    pub(crate) fn owns(&self, request: PanelGeneration) -> bool {
        request.panel_id() == self.id
    }

    pub(crate) fn accepts(&self, request: PanelGeneration) -> bool {
        self.owns(request) && self.generation_clock.is_current(request.generation())
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected_track = None;
        self.selected_presentation_nodes.clear();
        self.presentation_selection_anchor = None;
        self.selected_record = None;
        self.selected_alignment_track = None;
        self.selected_alignment_tracks.clear();
        self.alignment_selection_anchor = None;
        self.selected_read_tracks.clear();
        self.read_selection_anchor = None;
        self.selected_read_event = None;
        self.selected_pileup_position = None;
        self.selected_modification_position = None;
        self.selected_junction = None;
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
        self.hovered_record = None;
        self.hover_inspector_target = None;
    }
}

impl ViewPanelController {
    pub(crate) fn effective_vertical_scroll_points(&self) -> f32 {
        (self.vertical_scroll_points - self.vertical_content_origin_points).max(0.0)
    }

    pub(crate) fn read_panel_scroll_state(&self, track_id: TrackId) -> ReadPanelScrollState {
        self.read_panel_scrolls
            .get(&track_id)
            .copied()
            .unwrap_or_default()
    }
}
