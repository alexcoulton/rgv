use super::super::*;
use crate::ui::ScrollbarOverview;

#[derive(Default)]
pub(crate) struct PendingQueries {
    pub(crate) alignment: bool,
    pub(crate) reference: bool,
    pub(crate) tracks: bool,
}

pub(crate) struct PanelScrollDisplayCache {
    pub(crate) scrollable: DisplayList,
    pub(crate) pinned: DisplayList,
    pub(crate) overview: ScrollbarOverview,
    pub(crate) content_bottom: f32,
    pub(crate) scroll_clip_top: f32,
    pub(crate) supports_fast_scroll: bool,
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
    pub(crate) scroll_display_cache: Option<PanelScrollDisplayCache>,
    pub(crate) reference_slice: Option<Arc<ReferenceSlice>>,
    pub(crate) reference_settled_generation: Option<u64>,
    pub(crate) pending_queries: PendingQueries,
    pub(crate) track_snapshots: Vec<TrackSnapshot>,
    pub(crate) track_snapshot_interval: Option<GenomicInterval>,
    pub(crate) track_settled_generation: Option<u64>,
    pub(crate) vertical_scroll_points: f32,
    pub(crate) content_bottom_points: f32,
    pub(crate) track_frames: Vec<TrackFrame>,
    pub(crate) selected_track: Option<TrackFrameKey>,
    pub(crate) last_gesture_query_at: Option<Instant>,
}

pub(crate) const INITIAL_PANEL_ID: PanelId = PanelId::new(1);

/// Ordered runtime panel collection.
///
/// Active-panel access is deliberately explicit. This keeps code which acts on
/// one viewport visually distinct from code which iterates over the workspace.
pub(crate) struct PanelWorkspace {
    pub(crate) panels: Vec<ViewPanelController>,
    pub(crate) active: usize,
}

impl PanelWorkspace {
    pub(crate) fn new(initial: ViewPanelController) -> Self {
        Self {
            panels: vec![initial],
            active: 0,
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
        Some(Self { panels, active })
    }

    pub(crate) fn from_session(
        session: &SessionDocument,
        total_width: f32,
        content_top: f32,
    ) -> Result<Self> {
        let panel_width = f64::from((total_width / session.panels.len().max(1) as f32).max(1.0));
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
                panel.content_bottom_points = content_top;
                Ok(panel)
            })
            .collect::<Result<Vec<_>>>()?;
        Self::from_ordered(panels, session.active_panel)
            .context("session active view panel is unavailable")
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

pub(crate) fn alignment_frame_bottom(
    frame_top: f32,
    natural_bottom: f32,
    shared_height: Option<f32>,
) -> f32 {
    shared_height.map_or(natural_bottom, |height| {
        natural_bottom.max(frame_top + height)
    })
}

pub(crate) fn maximum_alignment_frame_height(
    panels: &PanelWorkspace,
    track_id: TrackId,
) -> Option<f32> {
    panels
        .iter()
        .filter_map(|panel| {
            panel
                .track_frames
                .iter()
                .find(|frame| frame.key == TrackFrameKey::Alignment(track_id))
                .map(|frame| (frame.bottom - frame.top).max(0.0))
        })
        .reduce(f32::max)
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
            scroll_display_cache: None,
            reference_slice: None,
            reference_settled_generation: None,
            pending_queries: PendingQueries::default(),
            track_snapshots: Vec::new(),
            track_snapshot_interval: None,
            track_settled_generation: None,
            vertical_scroll_points: 0.0,
            content_bottom_points: 0.0,
            track_frames: Vec::new(),
            selected_track: None,
            last_gesture_query_at: None,
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
        self.selected_record = None;
        self.selected_alignment_track = None;
        self.selected_alignment_tracks.clear();
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
