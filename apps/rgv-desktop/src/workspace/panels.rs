use super::super::*;
use crate::ui::{ScrollbarOverview, WorkspacePaneLayout};

#[derive(Default)]
pub(crate) struct PendingQueries {
    pub(crate) alignment: bool,
    pub(crate) reference: bool,
    pub(crate) tracks: bool,
}

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
    pub(crate) materialized_row_range: Option<(usize, usize)>,
    pub(crate) total_rows: Option<usize>,
    /// Optional semantic authority for the read navigator miniature. `Some(empty)` is
    /// deliberately distinct from `None`: a unified scene can truthfully report that the
    /// current genomic camera has no occupied rows; `None` means row authority is unavailable.
    pub(crate) authoritative_overview: Option<ScrollbarOverview>,
    pub(crate) overview: ScrollbarOverview,
    pub(crate) actual_content_bottom: f32,
    pub(crate) clip_top: f32,
    pub(crate) clip_bottom: f32,
}

/// Identity of the immutable scene row atlas used by a vertical read camera.
pub(crate) type ReadRowCoordinateSpace = RowCoordinateSpaceRevision;

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StableRowCameraAnchor {
    pub(crate) row: usize,
    pub(crate) offset_fraction: f32,
    pub(crate) coordinate_space: ReadRowCoordinateSpace,
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
    /// Contig whose row coordinates currently own this scroll state. A row number has no
    /// meaning on another contig and must never leak into its scene camera.
    pub(crate) scene_contig: Option<ContigId>,
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

impl ReadPanelScrollState {
    pub(crate) fn effective_scroll_points(self) -> f32 {
        (self.scroll_points - self.content_origin_points).max(0.0)
    }

    /// Binds this state to a scene-row authority. Density is a user preference and survives;
    /// row coordinates, content bounds, and latent scrollbar fractions do not.
    pub(crate) fn bind_scene_contig(
        &mut self,
        contig: ContigId,
        force_discontinuity: bool,
    ) -> bool {
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
    pub(crate) fn retire_scene_authority(&mut self) -> bool {
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
    pub(crate) selected_feature: Option<FeatureSelection>,
    pub(crate) selected_variant: Option<VariantSelection>,
    pub(crate) selected_gwas: Option<GwasSelection>,
    pub(crate) selected_quantitative: Option<QuantitativeSelection>,
    pub(crate) selected_copy_number: Option<CopyNumberSelection>,
    pub(crate) selected_mutation: Option<MutationSelection>,
    pub(crate) selected_interaction: Option<InteractionSelection>,
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
    pub(crate) reference_slice: Option<Arc<ReferenceSlice>>,
    pub(crate) reference_slice_revision: Option<ReferenceRevision>,
    pub(crate) reference_slice_generation: Option<u64>,
    /// Exact authority requested from the FASTA actor for this panel generation. Alignment Base
    /// scenes can require a wider interval than the ordinary visible reference row.
    pub(crate) reference_request_revision: Option<ReferenceRevision>,
    pub(crate) reference_request_interval: Option<GenomicInterval>,
    /// Owner-specific FASTA installs accepted by bounded alignment-engine control queues. These
    /// distinguish an authority already queued for one live lane from another lane which still
    /// needs either a wider slice or its own sequential query.
    pub(crate) reference_installs_queued:
        BTreeMap<SourceOwnerId, (ReferenceRevision, GenomicInterval)>,
    pub(crate) reference_settled_generation: Option<u64>,
    pub(crate) pending_queries: PendingQueries,
    pub(crate) track_snapshots: Vec<TrackSnapshot>,
    /// Buffered source horizon owned independently by each contextual track snapshot.
    pub(crate) track_snapshot_intervals: BTreeMap<TrackId, GenomicInterval>,
    pub(crate) track_snapshot_generations: BTreeMap<TrackId, u64>,
    /// Exact buffered horizon of the currently queued contextual-track request.
    pub(crate) track_request_interval: Option<GenomicInterval>,
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
    /// Per-lane terminal failures for the current contextual query generation.
    pub(crate) track_query_error_generations: BTreeMap<TrackId, u64>,
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
                                scene_contig: None,
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
            selected_alignment_track: None,
            selected_alignment_tracks: Vec::new(),
            alignment_selection_anchor: None,
            selected_read_tracks: Vec::new(),
            read_selection_anchor: None,
            selected_pileup_position: None,
            selected_modification_position: None,
            selected_junction: None,
            selected_junction_component: None,
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
            pending_alignment_reveal: None,
            hovered_alignment_hit: None,
            selected_alignment_hit: None,
            hover_inspector_target: None,
            hovered_junction_arc: None,
            display_list: DisplayList::default(),
            generic_display_list: DisplayList::default(),
            context_scene: PanelContextScene::default(),
            scroll_display_cache: None,
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
            track_query_error_generations: BTreeMap::new(),
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

    /// Settles the current panel generation after the FASTA actor fails to establish any open
    /// revision. No prior slice or request can remain authoritative after that terminal failure.
    pub(crate) fn settle_reference_open_failure(&mut self) {
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
