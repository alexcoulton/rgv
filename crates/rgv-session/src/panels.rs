//! The ordered set of genomic view panels and the device-free state of each.
//!
//! [`PanelState`] is what navigation, history and query bookkeeping need of a panel. A host that
//! draws panels wraps it in its own type (display caches, layout rectangles, GPU leases) and
//! implements [`Panel`]; [`PanelWorkspace`] and the action reducer are generic over that type, so
//! the same code runs over bare [`PanelState`]s in a headless session and in tests.

use crate::navigation::{IMPORTED_PLACEHOLDER_LOCUS, ImportedLocusRequest, standalone_locus};
use crate::navigation_transaction::{NavigationPresentation, NavigationQuerySchedule};
use crate::selection::{
    CopyNumberSelection, FeatureSelection, GwasSelection, InteractionSelection, MutationSelection,
    QuantitativeSelection, VariantSelection,
};
use anyhow::{Context as _, Result};
use rgv_alignment_scene::{ReferenceRevision, RowCoordinateSpaceRevision};
use rgv_core::{
    ContigId, GenerationClock, GenomicInterval, MAXIMUM_VIEW_PANELS, NamedNavigationHistory,
    NavigationEntry, PanelGeneration, PanelId, SessionDocument, SessionLocus,
    SessionReadPanelState, SessionViewPanel, TrackId, Viewport,
};
use rgv_engine::{SourceOwnerId, TrackSnapshot};
use rgv_hts::ReferenceSlice;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub struct PendingQueries {
    pub alignment: bool,
    pub reference: bool,
    pub tracks: bool,
}

/// Identity of the immutable scene row atlas used by a vertical read camera.
pub type ReadRowCoordinateSpace = RowCoordinateSpaceRevision;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StableRowCameraAnchor {
    pub row: usize,
    pub offset_fraction: f32,
    pub coordinate_space: ReadRowCoordinateSpace,
    pub row_height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReadPanelScrollState {
    pub scroll_points: f32,
    pub content_origin_points: f32,
    pub latent_scroll_fraction: Option<f32>,
    pub content_bottom_points: f32,
    /// Density selected through this workspace's read navigator. `None` uses
    /// the track's configured default density.
    pub density: Option<f32>,
    /// Stable row at the top of the viewport plus the fractional offset into
    /// it. This survives changes in total stack height and row density.
    pub stable_row_anchor: Option<StableRowCameraAnchor>,
    /// Contig whose row coordinates currently own this scroll state. A row number has no
    /// meaning on another contig and must never leak into its scene camera.
    pub scene_contig: Option<ContigId>,
}

impl ReadPanelScrollState {
    pub fn effective_scroll_points(self) -> f32 {
        (self.scroll_points - self.content_origin_points).max(0.0)
    }

    /// Binds this state to a scene-row authority. Density is a user preference and survives;
    /// row coordinates, content bounds, and latent scrollbar fractions do not.
    pub fn bind_scene_contig(&mut self, contig: ContigId, force_discontinuity: bool) -> bool {
        let changed =
            force_discontinuity || self.scene_contig.is_some_and(|current| current != contig);
        if changed {
            let density = self.density;
            *self = Self {
                density,
                scene_contig: Some(contig),
                ..Self::default()
            };
        } else {
            self.scene_contig = Some(contig);
        }
        changed
    }

    /// Ends the row-coordinate authority owned by a detailed read scene. Density is a user
    /// preference and survives the transition, but an index-only overview has no read rows,
    /// content extent, or latent scrollbar position to retain.
    pub fn retire_scene_authority(&mut self) -> bool {
        let changed = self.scroll_points != 0.0
            || self.content_origin_points != 0.0
            || self.latent_scroll_fraction.is_some()
            || self.content_bottom_points != 0.0
            || self.stable_row_anchor.is_some()
            || self.scene_contig.is_some();
        let density = self.density;
        *self = Self {
            density,
            ..Self::default()
        };
        changed
    }
}

/// The device-free state of one genomic view panel: identity, camera, history, query
/// generation and the retained query results. Nothing here names a window, GPU or widget type.
pub struct PanelState {
    pub id: PanelId,
    pub viewport: Viewport,
    pub generation_clock: GenerationClock,
    /// Keyed by contig name: a contig id is only meaningful in one dictionary.
    pub navigation_history: NamedNavigationHistory,
    pub home: NavigationEntry,
    pub requested_locus: Option<String>,
    pub contig_name: String,
    pub contig_length: u64,
    pub reference_slice: Option<Arc<ReferenceSlice>>,
    pub reference_slice_revision: Option<ReferenceRevision>,
    pub reference_slice_generation: Option<u64>,
    /// Exact authority requested from the FASTA actor for this panel generation. Alignment Base
    /// scenes can require a wider interval than the ordinary visible reference row.
    pub reference_request_revision: Option<ReferenceRevision>,
    pub reference_request_interval: Option<GenomicInterval>,
    /// Owner-specific FASTA installs accepted by bounded alignment-engine control queues. These
    /// distinguish an authority already queued for one live lane from another lane which still
    /// needs either a wider slice or its own sequential query.
    pub reference_installs_queued: BTreeMap<SourceOwnerId, (ReferenceRevision, GenomicInterval)>,
    pub reference_settled_generation: Option<u64>,
    pub pending_queries: PendingQueries,
    pub track_snapshots: Vec<TrackSnapshot>,
    /// Buffered source horizon owned independently by each contextual track snapshot.
    pub track_snapshot_intervals: BTreeMap<TrackId, GenomicInterval>,
    pub track_snapshot_generations: BTreeMap<TrackId, u64>,
    /// Exact buffered horizon of the currently queued contextual-track request.
    pub track_request_interval: Option<GenomicInterval>,
    pub track_settled_generation: Option<u64>,
    pub vertical_scroll_points: f32,
    /// Position of the laid-out scrollable content inside the density-expanded
    /// vertical world. Rendering uses `vertical_scroll_points - origin`, while
    /// the scrollbar and overview use the real world-space camera position.
    pub vertical_content_origin_points: f32,
    /// Conventional parent scroll for an overflowing stack of read panels.
    /// Per-panel read/data scroll remains in `read_panel_scrolls`.
    pub read_stack_scroll_points: f32,
    pub read_stack_content_bottom_points: f32,
    /// Normalized vertical camera retained while the content has no scroll
    /// range, so a density gesture does not snap its thumb back to the top.
    pub latent_vertical_scroll_fraction: Option<f32>,
    /// Independent vertical cameras for the read panel belonging to each
    /// alignment track. The legacy scalar fields above mirror the primary
    /// track for backward-compatible sessions and automation.
    pub read_panel_scrolls: BTreeMap<TrackId, ReadPanelScrollState>,
    pub content_bottom_points: f32,
    pub navigation_presentation: NavigationPresentation,
    pub navigation_query_schedule: NavigationQuerySchedule,
    pub reference_query_error_generation: Option<u64>,
    /// Per-lane terminal failures for the current contextual query generation.
    pub track_query_error_generations: BTreeMap<TrackId, u64>,
    /// The record selected in an auxiliary lane, by kind. Each indexes into the lane's
    /// current snapshot in `track_snapshots` and follows its record when the lane refreshes
    /// ([`crate::selection::reresolve_auxiliary_selections`]).
    pub selected_feature: Option<FeatureSelection>,
    pub selected_variant: Option<VariantSelection>,
    pub selected_gwas: Option<GwasSelection>,
    pub selected_quantitative: Option<QuantitativeSelection>,
    pub selected_copy_number: Option<CopyNumberSelection>,
    pub selected_mutation: Option<MutationSelection>,
    pub selected_interaction: Option<InteractionSelection>,
}

/// A panel as the panel workspace and the action reducer see it.
pub trait Panel: Sized {
    fn state(&self) -> &PanelState;
    fn state_mut(&mut self) -> &mut PanelState;
    /// Wraps a freshly created state in the host's panel type.
    fn from_state(state: PanelState) -> Self;

    /// The alignment track the panel's pileup and modification selections belong to. A host
    /// without alignment selection (a headless session) keeps the default.
    fn selected_alignment_track(&self) -> Option<TrackId> {
        None
    }

    /// The selected pileup column, captured into the owning track's coverage lane.
    fn selected_pileup_position(&self) -> Option<u64> {
        None
    }

    /// The selected modification column, captured into the owning track's modification lane.
    fn selected_modification_position(&self) -> Option<u64> {
        None
    }

    /// Drops a pending read reveal whose scene authority has just ended: the one aimed at
    /// `only_track`, or any pending reveal when `None`.
    fn clear_pending_alignment_reveal(&mut self, _only_track: Option<TrackId>) {}
}

impl Panel for PanelState {
    fn state(&self) -> &PanelState {
        self
    }

    fn state_mut(&mut self) -> &mut PanelState {
        self
    }

    fn from_state(state: PanelState) -> Self {
        state
    }
}

impl PanelState {
    /// Records `interval` on the panel's current contig as a history visit.
    /// Set `contig_name` first when the navigation changes contig.
    pub fn visit_history(&mut self, interval: GenomicInterval) {
        self.navigation_history
            .visit(NavigationEntry::new(self.contig_name.clone(), interval));
    }

    /// The reference source changed: drops the slice and every reference request in flight,
    /// and asks again for the current locus under a new generation.
    pub fn rerequest_reference(&mut self) {
        let interval = self.viewport.interval();
        self.requested_locus = Some(format!(
            "{}:{}-{}",
            self.contig_name,
            interval.start().saturating_add(1),
            interval.end()
        ));
        self.generation_clock.advance();
        self.reference_slice = None;
        self.reference_slice_revision = None;
        self.reference_slice_generation = None;
        self.reference_request_revision = None;
        self.reference_request_interval = None;
        self.reference_installs_queued.clear();
        self.reference_settled_generation = None;
        self.reference_query_error_generation = None;
        self.pending_queries.reference = false;
    }

    /// Restarts history and Home at `interval` on the current contig.
    pub fn reset_history(&mut self, interval: GenomicInterval) {
        self.home = NavigationEntry::new(self.contig_name.clone(), interval);
        self.navigation_history = NamedNavigationHistory::new(self.home.clone());
    }

    pub fn new(
        id: PanelId,
        viewport: Viewport,
        requested_locus: Option<String>,
        contig_name: String,
        contig_length: u64,
    ) -> Self {
        let initial_interval = viewport.interval();
        let home = NavigationEntry::new(contig_name.clone(), initial_interval);
        Self {
            id,
            viewport,
            generation_clock: GenerationClock::default(),
            navigation_history: NamedNavigationHistory::new(home.clone()),
            home,
            requested_locus,
            contig_name,
            contig_length,
            reference_slice: None,
            reference_slice_revision: None,
            reference_slice_generation: None,
            reference_request_revision: None,
            reference_request_interval: None,
            reference_installs_queued: BTreeMap::new(),
            reference_settled_generation: None,
            pending_queries: PendingQueries::default(),
            track_snapshots: Vec::new(),
            track_snapshot_intervals: BTreeMap::new(),
            track_snapshot_generations: BTreeMap::new(),
            track_request_interval: None,
            track_settled_generation: None,
            vertical_scroll_points: 0.0,
            vertical_content_origin_points: 0.0,
            read_stack_scroll_points: 0.0,
            read_stack_content_bottom_points: 0.0,
            latent_vertical_scroll_fraction: None,
            read_panel_scrolls: BTreeMap::new(),
            content_bottom_points: 0.0,
            navigation_presentation: NavigationPresentation::initial(
                initial_interval,
                0,
                Instant::now(),
            ),
            navigation_query_schedule: NavigationQuerySchedule::default(),
            reference_query_error_generation: None,
            track_query_error_generations: BTreeMap::new(),
            selected_feature: None,
            selected_variant: None,
            selected_gwas: None,
            selected_quantitative: None,
            selected_copy_number: None,
            selected_mutation: None,
            selected_interaction: None,
        }
    }

    pub fn request(&self) -> PanelGeneration {
        PanelGeneration::new(self.id, self.generation_clock.current())
    }

    pub fn owns(&self, request: PanelGeneration) -> bool {
        request.panel_id() == self.id
    }

    pub fn accepts(&self, request: PanelGeneration) -> bool {
        self.owns(request) && self.generation_clock.is_current(request.generation())
    }

    /// Settles the current panel generation after the FASTA actor fails to establish any open
    /// revision. No prior slice or request can remain authoritative after that terminal failure.
    pub fn settle_reference_open_failure(&mut self) {
        self.reference_slice = None;
        self.reference_slice_revision = None;
        self.reference_slice_generation = None;
        self.reference_request_revision = None;
        self.reference_request_interval = None;
        self.reference_installs_queued.clear();
        self.pending_queries.reference = false;
        let generation = self.generation_clock.current().get();
        self.reference_settled_generation = Some(generation);
        self.reference_query_error_generation = Some(generation);
    }

    pub fn effective_vertical_scroll_points(&self) -> f32 {
        (self.vertical_scroll_points - self.vertical_content_origin_points).max(0.0)
    }

    pub fn read_panel_scroll_state(&self, track_id: TrackId) -> ReadPanelScrollState {
        self.read_panel_scrolls
            .get(&track_id)
            .copied()
            .unwrap_or_default()
    }
}

pub const INITIAL_PANEL_ID: PanelId = PanelId::new(1);

/// Ordered runtime panel collection.
///
/// Active-panel access is deliberately explicit. This keeps code which acts on
/// one viewport visually distinct from code which iterates over the workspace.
pub struct PanelWorkspace<P = PanelState> {
    pub panels: Vec<P>,
    pub active: usize,
    /// One annotation camera is shared by every genomic panel. The left gutter
    /// describes workspace rows, so a per-panel offset would make that gutter
    /// truthful for at most one locus at a time.
    pub annotation_scroll_points: f32,
}

impl<P: Panel> PanelWorkspace<P> {
    pub fn new(initial: P) -> Self {
        Self {
            panels: vec![initial],
            active: 0,
            annotation_scroll_points: 0.0,
        }
    }

    pub fn from_ordered(panels: Vec<P>, active_id: PanelId) -> Option<Self> {
        if panels.is_empty()
            || panels.len() > MAXIMUM_VIEW_PANELS
            || panels
                .iter()
                .map(|panel| panel.state().id)
                .collect::<std::collections::HashSet<_>>()
                .len()
                != panels.len()
        {
            return None;
        }
        let active = panels
            .iter()
            .position(|panel| panel.state().id == active_id)?;
        Some(Self {
            panels,
            active,
            annotation_scroll_points: 0.0,
        })
    }

    pub fn from_session(
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
                let mut panel = PanelState::new(
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
                                scene_contig: None,
                            },
                        )
                    })
                    .collect();
                panel.content_bottom_points = content_top;
                Ok(P::from_state(panel))
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

    /// Replaces the placeholder locus of an imported IGV session in every
    /// panel: each shows a neutral standalone window and asks for `request`
    /// once a dictionary can place it.
    pub fn adopt_imported_locus(&mut self, request: &ImportedLocusRequest) -> Result<()> {
        let (interval, contig_name, contig_length) = standalone_locus(IMPORTED_PLACEHOLDER_LOCUS)?;
        for panel in &mut self.panels {
            let panel = panel.state_mut();
            panel.viewport = Viewport::new(interval, panel.viewport.pixel_width())?;
            panel.contig_name.clone_from(&contig_name);
            panel.contig_length = contig_length;
            panel.requested_locus = request.requested_locus();
            panel.reset_history(interval);
        }
        Ok(())
    }

    /// A workspace always holds at least one panel, so there is no `is_empty`.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.panels.len()
    }

    pub fn active(&self) -> &P {
        &self.panels[self.active]
    }

    pub fn active_mut(&mut self) -> &mut P {
        &mut self.panels[self.active]
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &P> {
        self.panels.iter()
    }

    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = &mut P> {
        self.panels.iter_mut()
    }

    pub fn get(&self, id: PanelId) -> Option<&P> {
        self.panels.iter().find(|panel| panel.state().id == id)
    }

    pub fn get_mut(&mut self, id: PanelId) -> Option<&mut P> {
        self.panels.iter_mut().find(|panel| panel.state().id == id)
    }

    /// The panel `id` names. Callers resolve the id at the UI or automation boundary and pass
    /// it down; an id that has left the workspace falls back to the focused panel so a late
    /// call can never index out of bounds.
    pub fn panel(&self, id: PanelId) -> &P {
        self.get(id).unwrap_or_else(|| &self.panels[self.active])
    }

    /// Mutable counterpart of [`Self::panel`].
    pub fn panel_mut(&mut self, id: PanelId) -> &mut P {
        let index = self
            .panels
            .iter()
            .position(|panel| panel.state().id == id)
            .unwrap_or(self.active);
        &mut self.panels[index]
    }

    /// The device-free state of every panel, in order.
    pub fn states(&self) -> impl ExactSizeIterator<Item = &PanelState> {
        self.panels.iter().map(Panel::state)
    }

    /// Mutable counterpart of [`Self::states`].
    pub fn states_mut(&mut self) -> impl ExactSizeIterator<Item = &mut PanelState> {
        self.panels.iter_mut().map(Panel::state_mut)
    }

    /// The device-free state of panel `id`, if it is in the workspace.
    pub fn state(&self, id: PanelId) -> Option<&PanelState> {
        self.get(id).map(Panel::state)
    }

    /// Mutable counterpart of [`Self::state`].
    pub fn state_mut(&mut self, id: PanelId) -> Option<&mut PanelState> {
        self.get_mut(id).map(Panel::state_mut)
    }

    /// The device-free state of the panel [`Self::panel`] resolves `id` to.
    pub fn panel_state(&self, id: PanelId) -> &PanelState {
        self.panel(id).state()
    }

    /// Mutable counterpart of [`Self::panel_state`].
    pub fn panel_state_mut(&mut self, id: PanelId) -> &mut PanelState {
        self.panel_mut(id).state_mut()
    }

    /// Whether `id` is the focused panel: the one keyboard and locus-box input address.
    pub fn is_active(&self, id: PanelId) -> bool {
        self.panels[self.active].state().id == id
    }

    pub fn next_id(&self) -> PanelId {
        PanelId::new(
            self.panels
                .iter()
                .map(|panel| panel.state().id.get())
                .max()
                .unwrap_or_default()
                .saturating_add(1),
        )
    }

    pub fn push(&mut self, panel: P) -> bool {
        if self.panels.len() >= MAXIMUM_VIEW_PANELS || self.get(panel.state().id).is_some() {
            return false;
        }
        self.panels.push(panel);
        true
    }

    pub fn activate(&mut self, id: PanelId) -> bool {
        let Some(index) = self.panels.iter().position(|panel| panel.state().id == id) else {
            return false;
        };
        self.active = index;
        true
    }

    pub fn move_panel(&mut self, id: PanelId, new_index: usize) -> bool {
        let Some(old_index) = self.panels.iter().position(|panel| panel.state().id == id) else {
            return false;
        };
        let active_id = self.panels[self.active].state().id;
        let panel = self.panels.remove(old_index);
        self.panels.insert(new_index.min(self.panels.len()), panel);
        self.active = self
            .panels
            .iter()
            .position(|panel| panel.state().id == active_id)
            .expect("active panel survives a reorder");
        true
    }

    pub fn remove(&mut self, id: PanelId) -> Option<P> {
        if self.panels.len() == 1 {
            return None;
        }
        let index = self
            .panels
            .iter()
            .position(|panel| panel.state().id == id)?;
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

/// The panel part of a session document: the focused panel's locus, every panel in order, and
/// which one has focus. This is the persisted projection of the panel workspace, so the action
/// tests compare it to decide whether two front ends left the same document.
pub fn session_panel_documents<P: Panel>(
    panels: &PanelWorkspace<P>,
) -> (SessionLocus, Vec<SessionViewPanel>, PanelId) {
    let locus_of = |panel: &PanelState| {
        let interval = panel.viewport.interval();
        SessionLocus {
            contig: panel.contig_name.clone(),
            start: interval.start(),
            end: interval.end(),
        }
    };
    let saved = panels
        .iter()
        .map(|panel| {
            let panel = panel.state();
            let mut saved = SessionViewPanel::new(panel.id, locus_of(panel));
            saved.vertical_scroll_points = panel.vertical_scroll_points;
            // Keep the existing per-panel session field as a compatibility
            // projection of the single workspace annotation camera.
            saved.annotation_scroll_points = panels.annotation_scroll_points;
            saved.vertical_content_origin_points = panel.vertical_content_origin_points;
            saved.read_stack_scroll_points = panel.read_stack_scroll_points;
            saved.read_panel_states = panel
                .read_panel_scrolls
                .iter()
                .map(|(track_id, state)| SessionReadPanelState {
                    track_id: *track_id,
                    scroll_points: state.scroll_points,
                    content_origin_points: state.content_origin_points,
                    latent_scroll_fraction: state.latent_scroll_fraction,
                    content_bottom_points: state.content_bottom_points,
                    density: state.density,
                })
                .collect();
            saved
        })
        .collect();
    (
        locus_of(panels.active().state()),
        saved,
        panels.active().state().id,
    )
}
