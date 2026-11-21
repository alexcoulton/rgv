//! Desktop side of the alignment coverage pyramid (ARCH-1).
//!
//! Each alignment track owns one [`AlignmentCoverageEngine`] beside its scene engine. Every
//! frame the desktop states each panel's camera to it; the engine publishes, per panel, the
//! best multi-resolution coverage it has and keeps refining it. This module decides, per
//! `(panel, track)`, which authority the coverage lane shows:
//!
//! - exact read-derived depth, when the committed scene covers the whole visible interval;
//! - the pyramid publication, at regional and overview zoom when it does not;
//! - the index-only placeholder at overview zoom until the first publication arrives.
//!
//! The decisions are pure functions so they can be tested without a window. The pyramid is a
//! stand-in the read path never depends on: nothing here changes scene acquisition.

use std::sync::Arc;

use rgv_alignment_scene::{CpuClipRect, CpuSummaryCamera, DepthLaneAuthority};
use rgv_core::{
    AlignmentAuxComparison, AlignmentAuxFilterValue, AlignmentFlagPredicate,
    AlignmentViewPredicate, ContigId, DetailLevel, GenomicInterval, PanelId, SessionFilters,
    TrackId, Viewport,
};
use rgv_engine::{
    AlignmentCoverageEngine, AlignmentCoverageEngineConfig, AlignmentCoveragePublication,
    AlignmentCoverageRequest, AlignmentCoverageState, SourceOwnerId,
};
use rgv_hts::{
    AlignmentFilter, AlignmentOpenOptions, AuxComparison, AuxPredicate, CoverageEligibility,
    CoverageSidecarCache, FilterValue, QueryOptions,
};
use rgv_render::{AlignmentDepthPyramidGpuRequest, AlignmentGpuScissor};

use crate::Viewer;
use crate::ui;
use crate::workspace::alignment_management::{
    alignment_clip_gpu_scissor, alignment_scene_covered_clip,
};
use crate::workspace::alignments::{ActiveFilters, AlignmentTrackLifecycle};

const SAM_FLAG_PAIRED: u16 = 0x1;
const SAM_FLAG_PROPER_PAIR: u16 = 0x2;
const SAM_FLAG_REVERSE: u16 = 0x10;
const SAM_FLAG_SECONDARY: u16 = 0x100;
const SAM_FLAG_FAILED_QC: u16 = 0x200;
const SAM_FLAG_DUPLICATE: u16 = 0x400;
const SAM_FLAG_SUPPLEMENTARY: u16 = 0x800;

/// What the coverage lane of one `(panel, track)` is drawn from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AlignmentCoverageMode {
    /// Exact depth derived from the committed read evidence. Also the mode at read and base
    /// zoom, where the pyramid is never shown.
    ReadEvidence,
    /// The pyramid at its target level over the whole visible interval.
    PyramidExact,
    /// The pyramid with a coarser, finer, partial or previous-camera stand-in: pending.
    PyramidCoarse,
    /// Overview zoom before the first pyramid publication: the index-only placeholder.
    IndexDensity,
    /// Regional zoom with neither exact depth nor a pyramid publication yet.
    Pending,
}

impl AlignmentCoverageMode {
    pub(crate) const fn draws_pyramid(self) -> bool {
        matches!(self, Self::PyramidExact | Self::PyramidCoarse)
    }

    /// Whether the lane must be presented as still loading.
    pub(crate) const fn is_pending(self) -> bool {
        matches!(
            self,
            Self::PyramidCoarse | Self::IndexDensity | Self::Pending
        )
    }

    /// Stable name reported by the automation workspace snapshot.
    pub(crate) const fn automation_name(self) -> &'static str {
        match self {
            Self::ReadEvidence => "read-evidence",
            Self::PyramidExact => "pyramid-exact",
            Self::PyramidCoarse => "pyramid-coarse",
            Self::IndexDensity => "index-density",
            Self::Pending => "pending",
        }
    }
}

/// The parts of a publication the mode decision depends on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct CoveragePublicationSummary {
    pub(crate) generation: u64,
    /// The pyramid, not read evidence, is the publication's authority.
    pub(crate) pyramid_authority: bool,
    /// The lane has at least one resident segment to draw.
    pub(crate) has_segments: bool,
    /// Every base the panel shows now is drawn from a tile of the level its zoom asks for.
    /// Judged against the current camera, not the one the lane was built for, so panning
    /// across resident tiles stays exact while a zoom to another level does not.
    pub(crate) exact_for_view: bool,
    pub(crate) failure: Option<Arc<str>>,
    /// Part of the visible coverage counts overlapping mates twice although the request asked
    /// for them to be counted once (the count-once window overflowed).
    pub(crate) mate_overlap_not_collapsed: bool,
}

impl CoveragePublicationSummary {
    /// Summarises `publication` against the request the panel currently states.
    pub(crate) fn of(
        publication: &AlignmentCoveragePublication,
        current: &AlignmentCoverageRequest,
    ) -> Self {
        let lane = publication.lane.as_deref();
        Self {
            generation: publication.generation,
            pyramid_authority: publication.authority == DepthLaneAuthority::Pyramid,
            has_segments: lane.is_some_and(|lane| !lane.segments().is_empty()),
            exact_for_view: lane.is_some_and(|lane| {
                let view = lane.view();
                let parameters = lane.parameters();
                let target_level = parameters
                    .level_for_bases_per_pixel(current.bases_per_pixel)
                    .min(parameters.root_level(view.contig_length));
                view.contig == current.contig
                    && pyramid_segments_exact_over(
                        lane.segments().iter().map(|segment| {
                            (segment.start, segment.end, segment.tile.tile().id().level)
                        }),
                        target_level,
                        current.start,
                        current.end.min(view.contig_length),
                    )
            }),
            failure: match &publication.state {
                AlignmentCoverageState::Failed(message) => Some(Arc::clone(message)),
                AlignmentCoverageState::DeferredToReadEvidence
                | AlignmentCoverageState::Refining { .. }
                | AlignmentCoverageState::Complete { .. } => None,
            },
            mate_overlap_not_collapsed: publication.mate_overlap_not_collapsed,
        }
    }
}

/// Whether ascending, disjoint `(start, end, level)` segments show every base of
/// `[start, end)` from `target_level`, with no gap and no coarser or finer stand-in.
pub(crate) fn pyramid_segments_exact_over(
    segments: impl IntoIterator<Item = (u64, u64, u8)>,
    target_level: u8,
    start: u64,
    end: u64,
) -> bool {
    let mut cursor = start;
    for (segment_start, segment_end, level) in segments {
        if cursor >= end {
            break;
        }
        if segment_end <= cursor {
            continue;
        }
        if segment_start > cursor || level != target_level {
            return false;
        }
        cursor = segment_end;
    }
    cursor >= end
}

/// Whether the pyramid may stand in at this zoom: regional and overview only. At read and base
/// zoom a pyramid bin is many pixels wide and read evidence is the only authority.
pub(crate) fn alignment_coverage_pyramid_zoom(bases_per_pixel: f64) -> bool {
    matches!(
        DetailLevel::for_bases_per_pixel(bases_per_pixel),
        DetailLevel::Overview | DetailLevel::Regional
    )
}

/// Chooses the coverage authority of one `(panel, track)`.
///
/// A publication answering another generation (a different contig or filter) is ignored: it
/// describes data the panel no longer asks for. A publication of the current generation built
/// for an earlier camera is drawn; it counts as complete only if it shows the whole current
/// view at the level the current zoom asks for.
pub(crate) fn select_alignment_coverage_mode(
    bases_per_pixel: f64,
    exact_read_depth_ready: bool,
    current_generation: u64,
    publication: Option<&CoveragePublicationSummary>,
) -> AlignmentCoverageMode {
    let detail = DetailLevel::for_bases_per_pixel(bases_per_pixel);
    let overview = detail == DetailLevel::Overview;
    if !alignment_coverage_pyramid_zoom(bases_per_pixel) || (!overview && exact_read_depth_ready) {
        return AlignmentCoverageMode::ReadEvidence;
    }
    let usable = publication.filter(|publication| {
        publication.generation == current_generation
            && publication.pyramid_authority
            && publication.has_segments
    });
    match usable {
        Some(publication) if publication.exact_for_view && publication.failure.is_none() => {
            AlignmentCoverageMode::PyramidExact
        }
        Some(_) => AlignmentCoverageMode::PyramidCoarse,
        None if overview => AlignmentCoverageMode::IndexDensity,
        None => AlignmentCoverageMode::Pending,
    }
}

/// The mode a lane presents when the GPU refused pyramid pages this frame: a pyramid that is
/// exact in memory but not wholly drawn is still pending, never complete.
pub(crate) const fn alignment_coverage_mode_after_gpu_refusal(
    mode: AlignmentCoverageMode,
    pages_refused: usize,
) -> AlignmentCoverageMode {
    match mode {
        AlignmentCoverageMode::PyramidExact if pages_refused > 0 => {
            AlignmentCoverageMode::PyramidCoarse
        }
        other => other,
    }
}

/// Label and detail line of the coverage lane. A pending lane never reads as complete, and a
/// complete pyramid lane whose depth counts overlapping mates twice (`mate_overlap_not_collapsed`)
/// says so instead of reading as the count-once coverage that was asked for.
pub(crate) fn alignment_coverage_lane_label(
    mode: AlignmentCoverageMode,
    failure: Option<&str>,
    unapplied_view_predicates: usize,
    mate_overlap_not_collapsed: bool,
) -> (String, Option<String>) {
    let label = if mode.is_pending() {
        "Coverage (loading…)"
    } else if mate_overlap_not_collapsed && mode.draws_pyramid() {
        "Coverage (overlapping mates counted twice)"
    } else {
        "Coverage"
    }
    .to_owned();
    let unapplied = (unapplied_view_predicates > 0 && mode.draws_pyramid()).then(|| {
        format!("{unapplied_view_predicates} view filter(s) not applied to coverage at this zoom")
    });
    let detail = match (mode, failure) {
        // Read evidence reports its own state; a pyramid failure is not this lane's concern.
        (AlignmentCoverageMode::ReadEvidence, _) | (AlignmentCoverageMode::Pending, None) => None,
        (_, Some(message)) => Some(format!("Coverage unavailable: {message}")),
        (AlignmentCoverageMode::PyramidExact, None) => unapplied,
        (AlignmentCoverageMode::PyramidCoarse, None) => Some(unapplied.map_or_else(
            || "Coarser coverage shown while it refines".to_owned(),
            |unapplied| format!("Coarser coverage shown while it refines · {unapplied}"),
        )),
        (AlignmentCoverageMode::IndexDensity, None) => {
            Some("Index-only overview until coverage arrives".to_owned())
        }
    };
    (label, detail)
}

/// The track's current filter settings in the terms the pyramid understands.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AlignmentCoverageEligibility {
    pub(crate) eligibility: CoverageEligibility,
    /// View predicates the pyramid cannot evaluate from a streamed record (template length,
    /// CIGAR events, substitutions, …). The lane says so while the pyramid is shown.
    pub(crate) unapplied_view_predicates: usize,
}

/// Derives the pyramid's eligibility from the same settings the read path filters by: MAPQ,
/// secondary/supplementary inclusion, SAM-flag predicates (duplicates, failed QC, pairing,
/// strand), auxiliary-tag predicates and the overlapping-mate policy. A change to any of them
/// changes the result, and so the pyramid.
pub(crate) fn alignment_coverage_eligibility(
    filters: &SessionFilters,
    active: &ActiveFilters,
) -> AlignmentCoverageEligibility {
    let mut options = QueryOptions {
        include_secondary: active.include_secondary,
        include_supplementary: active.include_supplementary,
        filter: AlignmentFilter {
            auxiliary: active.alignment.auxiliary.clone(),
            ..AlignmentFilter::default()
        },
        overlapping_mate_policy: filters.overlapping_mate_policy,
        ..QueryOptions::default()
    };
    options.filter.minimum_mapping_quality = active.alignment.minimum_mapping_quality;
    options.filter.required_flags = active.alignment.required_flags;
    options.filter.excluded_flags = active.alignment.excluded_flags;
    let mut unapplied = 0_usize;
    // The interactive chips are carried by `active` above; these are the saved view predicates.
    for predicate in &filters.alignment_view_filter.all {
        match predicate {
            AlignmentViewPredicate::MappingQuality {
                minimum,
                maximum: None,
            } => {
                options.filter.minimum_mapping_quality =
                    options.filter.minimum_mapping_quality.max(*minimum);
            }
            AlignmentViewPredicate::Flag { flag, expected } => {
                let bit = match flag {
                    AlignmentFlagPredicate::Duplicate => Some(SAM_FLAG_DUPLICATE),
                    AlignmentFlagPredicate::Secondary => Some(SAM_FLAG_SECONDARY),
                    AlignmentFlagPredicate::Supplementary => Some(SAM_FLAG_SUPPLEMENTARY),
                    AlignmentFlagPredicate::FailedQualityControl => Some(SAM_FLAG_FAILED_QC),
                    AlignmentFlagPredicate::Paired => Some(SAM_FLAG_PAIRED),
                    AlignmentFlagPredicate::ProperPair => Some(SAM_FLAG_PROPER_PAIR),
                    AlignmentFlagPredicate::ReverseStrand => Some(SAM_FLAG_REVERSE),
                    // A derived state, not one SAM bit for every record.
                    AlignmentFlagPredicate::MateMapped => None,
                };
                match (bit, *expected) {
                    (Some(SAM_FLAG_SECONDARY), false) => options.include_secondary = false,
                    (Some(SAM_FLAG_SUPPLEMENTARY), false) => options.include_supplementary = false,
                    (Some(bit), true) => options.filter.required_flags |= bit,
                    (Some(bit), false) => options.filter.excluded_flags |= bit,
                    (None, _) => unapplied += 1,
                }
            }
            AlignmentViewPredicate::Auxiliary {
                tag,
                comparison,
                value,
            } => options.filter.auxiliary.push(AuxPredicate {
                tag: *tag,
                comparison: match comparison {
                    AlignmentAuxComparison::Exists => AuxComparison::Exists,
                    AlignmentAuxComparison::Equal => AuxComparison::Equal,
                    AlignmentAuxComparison::NotEqual => AuxComparison::NotEqual,
                    AlignmentAuxComparison::Greater => AuxComparison::Greater,
                    AlignmentAuxComparison::GreaterOrEqual => AuxComparison::GreaterOrEqual,
                    AlignmentAuxComparison::Less => AuxComparison::Less,
                    AlignmentAuxComparison::LessOrEqual => AuxComparison::LessOrEqual,
                },
                value: value.as_ref().map(|value| match value {
                    AlignmentAuxFilterValue::Integer(value) => FilterValue::Integer(*value),
                    AlignmentAuxFilterValue::FloatBits(bits) => {
                        FilterValue::Float(f64::from_bits(*bits))
                    }
                    AlignmentAuxFilterValue::String(value) => FilterValue::String(value.clone()),
                }),
            }),
            // Template length, mate contig, CIGAR events, aligned length, substitutions and
            // bounded MAPQ ranges need more than the pyramid's streamed eligibility rule.
            _ => unapplied += 1,
        }
    }
    AlignmentCoverageEligibility {
        eligibility: CoverageEligibility::from_query_options(&options),
        unapplied_view_predicates: unapplied,
    }
}

/// Builds the request one panel states for one track. The generation advances only when the
/// data identity changes (contig or eligibility), so a publication for an earlier camera of the
/// same data is still recognisably the same generation, while one for other data is not.
pub(crate) fn alignment_coverage_request(
    previous: Option<&AlignmentCoverageRequest>,
    interval: GenomicInterval,
    bases_per_pixel: f64,
    eligibility: CoverageEligibility,
    exact_read_depth_ready: bool,
) -> AlignmentCoverageRequest {
    let generation = match previous {
        Some(previous)
            if previous.contig == interval.contig() && previous.eligibility == eligibility =>
        {
            previous.generation
        }
        Some(previous) => previous.generation.saturating_add(1),
        None => 1,
    };
    AlignmentCoverageRequest {
        generation,
        contig: interval.contig(),
        start: interval.start(),
        end: interval.end(),
        bases_per_pixel,
        eligibility,
        exact_read_depth_ready,
    }
}

/// Exact read-derived depth is resident and current for the whole visible interval.
pub(crate) fn exact_read_depth_ready(
    scene_contracts_current: bool,
    has_snapshot: bool,
    covered_interval: Option<GenomicInterval>,
    visible: GenomicInterval,
) -> bool {
    scene_contracts_current
        && has_snapshot
        && covered_interval.is_some_and(|covered| covered.contains_interval(visible))
}

/// Depth drawn at full lane height: the fixed maximum when the track sets one, otherwise the
/// publication's own maximum over the visible span, exactly as the exact lane autoscales.
pub(crate) fn alignment_coverage_pyramid_scale_maximum(
    fixed_maximum: Option<u32>,
    visible_maximum_depth: u32,
) -> u32 {
    fixed_maximum.unwrap_or(visible_maximum_depth).max(1)
}

/// The coverage subtrack's clip in the final annotation pane layout, or `None` while the lane
/// is hidden, collapsed or scrolled out of view.
pub(crate) fn alignment_coverage_lane_clip(
    layout: &ui::PaneLayoutSnapshot,
    track_id: TrackId,
    horizontal_x: f64,
    horizontal_width: f64,
) -> Option<CpuClipRect> {
    let item = layout.items.iter().find(|item| {
        item.frame.key == ui::TrackFrameKey::Alignment(track_id)
            && item.frame.channel == ui::TrackPane::Annotation
    })?;
    if !item.effective_content_visible || item.clip_rect.height() <= 0.0 || horizontal_width <= 0.0
    {
        return None;
    }
    let subtrack = item.frame.subtracks.iter().find(|subtrack| {
        !subtrack.collapsed && subtrack.alignment_subtrack == Some(ui::AlignmentSubtrack::Coverage)
    })?;
    let top = (subtrack.top - layout.scroll_points).max(item.clip_rect.top());
    let bottom = (subtrack.bottom - layout.scroll_points).min(item.clip_rect.bottom());
    (bottom > top).then_some(CpuClipRect {
        x: horizontal_x,
        y: f64::from(top),
        width: horizontal_width,
        height: f64::from(bottom - top),
    })
}

/// The parts of `clip` the exact lane does not draw: all of it without exact coverage, else
/// what lies left and right of the exactly covered span. The pyramid is drawn only there, so
/// the two authorities never overlap.
pub(crate) fn alignment_coverage_pyramid_clips(
    clip: CpuClipRect,
    exact_clip: Option<CpuClipRect>,
) -> Vec<CpuClipRect> {
    let Some(exact) = exact_clip else {
        return vec![clip];
    };
    let right = clip.x + clip.width;
    let exact_left = exact.x.clamp(clip.x, right);
    let exact_right = (exact.x + exact.width).clamp(exact_left, right);
    let mut clips = Vec::with_capacity(2);
    if exact_left - clip.x >= 1.0 {
        clips.push(CpuClipRect {
            width: exact_left - clip.x,
            ..clip
        });
    }
    if right - exact_right >= 1.0 {
        clips.push(CpuClipRect {
            x: exact_right,
            width: right - exact_right,
            ..clip
        });
    }
    clips
}

/// Per-panel coverage state of one track.
#[derive(Default)]
pub(crate) struct AlignmentCoverageProjection {
    /// The request last stated to the engine; `None` while the panel asks for nothing.
    pub(crate) request: Option<AlignmentCoverageRequest>,
    /// Last generation used, kept across releases so generations never repeat for an owner.
    pub(crate) last_generation: u64,
    pub(crate) unapplied_view_predicates: usize,
    /// Mode and failure the display was last built from; a change rebuilds the display.
    pub(crate) presented: Option<(AlignmentCoverageMode, Option<Arc<str>>)>,
    /// Whether the presented pyramid counts overlapping mates twice; part of the lane label.
    pub(crate) mate_overlap_not_collapsed: bool,
}

/// One coverage owner per panel within a track's engine.
pub(crate) const fn alignment_coverage_owner(panel_id: PanelId) -> SourceOwnerId {
    SourceOwnerId::new(panel_id.get())
}

/// Spawns the coverage worker of one alignment source. `None` when the thread cannot be
/// started; the track then simply has no pyramid.
pub(crate) fn spawn_alignment_coverage_engine(
    path: std::path::PathBuf,
    open_options: AlignmentOpenOptions,
    ledger: rgv_alignment_scene::AllocationLedger,
    sidecar: Option<Arc<CoverageSidecarCache>>,
) -> Option<AlignmentCoverageEngine> {
    let mut config = AlignmentCoverageEngineConfig::new(path, open_options, ledger);
    config.sidecar = sidecar;
    match AlignmentCoverageEngine::spawn(config) {
        Ok(engine) => Some(engine),
        Err(error) => {
            tracing::warn!(%error, "could not start the alignment coverage worker");
            None
        }
    }
}

/// Stops a coverage worker without blocking the caller: the worker is told to stop and a reaper
/// thread, whose handle is returned, waits for it.
pub(crate) fn retire_alignment_coverage_engine(
    engine: AlignmentCoverageEngine,
) -> Option<std::thread::JoinHandle<()>> {
    engine.retire()
}

/// Deletes every coverage tile under `root` through the cache's own [`CoverageSidecarCache::clear`]:
/// only tile files inside the cache's key directories, then the directories that became empty.
/// Returns the bytes removed.
pub(crate) fn clear_coverage_sidecar_directory(root: &std::path::Path) -> std::io::Result<u64> {
    CoverageSidecarCache::open(root, rgv_hts::COVERAGE_SIDECAR_DEFAULT_MAXIMUM_BYTES)?.clear()
}

/// One pyramid lane to draw this frame, owning what the GPU request borrows.
struct OwnedPyramidDraw {
    publication: Arc<AlignmentCoveragePublication>,
    camera: CpuSummaryCamera,
    scissor: AlignmentGpuScissor,
    scale_maximum: u32,
    transform: rgv_core::CoverageScaleTransform,
}

impl Viewer {
    /// The shared sidecar cache, opened on first use while the setting allows it.
    pub(crate) fn alignment_coverage_sidecar(&mut self) -> Option<Arc<CoverageSidecarCache>> {
        if !self.settings.coverage_cache_enabled {
            return None;
        }
        if self.alignment_coverage_sidecar.is_none() {
            self.alignment_coverage_sidecar = CoverageSidecarCache::open_default().map(Arc::new);
        }
        self.alignment_coverage_sidecar.clone()
    }

    /// Replaces every track's coverage worker, picking up the current cache setting and
    /// dropping all resident tiles. Old workers are retired without blocking.
    pub(crate) fn respawn_alignment_coverage_engines(&mut self) {
        let sidecar = self.alignment_coverage_sidecar();
        let reference_path = self
            .reference_source
            .as_ref()
            .map(crate::sources::source_path);
        let remote_reference_index_url = self.remote_reference_index_url.clone();
        let ledger = self.alignment_scene_ledger.clone();
        for index in 0..self.alignment_tracks.len() {
            let (path, open_options) = crate::workspace::alignments::alignment_source_open_request(
                &self.alignment_tracks[index].config,
                reference_path.clone(),
                remote_reference_index_url.clone(),
            );
            let replacement = spawn_alignment_coverage_engine(
                path,
                open_options,
                ledger.clone(),
                sidecar.clone(),
            );
            let track = &mut self.alignment_tracks[index];
            track.coverage_publication_sequence = 0;
            for projection in track.projections.values_mut() {
                projection.coverage.request = None;
                projection.coverage.presented = None;
            }
            if let Some(retired) = std::mem::replace(&mut track.coverage_engine, replacement)
                && let Some(task) = retire_alignment_coverage_engine(retired)
            {
                self.alignment_coverage_retirement_tasks.push(task);
            }
        }
        self.alignment_depth_pyramid_gpu_frame = None;
        self.mark_display_dirty("alignment coverage workers replaced");
    }

    /// Applies a change of the coverage-cache setting to the running session.
    pub(crate) fn apply_coverage_cache_setting(&mut self) {
        if !self.settings.coverage_cache_enabled {
            self.alignment_coverage_sidecar = None;
        }
        self.respawn_alignment_coverage_engines();
    }

    /// "Clear caches": deletes the persisted coverage tiles and drops the resident ones.
    pub(crate) fn clear_alignment_coverage_caches(&mut self) {
        // The open cache clears itself (and recounts); with the cache closed, the default
        // directory is cleared the same way.
        let cleared = match &self.alignment_coverage_sidecar {
            Some(cache) => Some(cache.clear()),
            None => rgv_hts::default_coverage_cache_directory()
                .filter(|root| root.is_dir())
                .map(|root| clear_coverage_sidecar_directory(&root)),
        };
        match cleared {
            Some(Ok(bytes)) => tracing::info!(bytes, "cleared the coverage cache"),
            Some(Err(error)) => crate::app::shell_state::report_error_to(
                &mut self.shell.notifications,
                &mut self.data_status,
                format!("Could not clear the coverage cache: {error}"),
            ),
            None => {}
        }
        self.alignment_depth_pyramid_gpu_renderer
            .discard_device_resources();
        self.respawn_alignment_coverage_engines();
    }

    /// The exactly covered interval of the committed scene for `(panel, track)`, when its
    /// contracts are current.
    fn alignment_exact_depth_interval(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<GenomicInterval> {
        let presented = self.presented_alignment_lane(panel_id, track_id)?;
        (presented.lane.scene_contracts_current && presented.lane.snapshot.is_some())
            .then_some(presented.lane.covered_interval)
            .flatten()
    }

    /// The panel's viewport in the track's own contig space, at the live panel width.
    fn alignment_coverage_viewport(
        &self,
        panel_id: PanelId,
        track_index: usize,
    ) -> Option<Viewport> {
        let panel = self.panels.get(panel_id)?;
        let dictionary = self
            .alignment_tracks
            .get(track_index)?
            .dictionary
            .as_ref()?;
        let contig: ContigId = dictionary.id_by_name(&panel.contig_name)?;
        let length = dictionary.get(contig)?.length();
        let viewport = panel.viewport.remapped_contig(contig, length).ok()?;
        match self.panel_column_rect(panel_id) {
            Some(rect) if rect.width() > 0.0 => viewport.resized(f64::from(rect.width())).ok(),
            _ => Some(viewport),
        }
    }

    /// Whether the track's coverage lane has a drawable clip in the panel's final pane layout.
    fn alignment_coverage_lane_on_screen(&self, panel_id: PanelId, track_id: TrackId) -> bool {
        self.panels.get(panel_id).is_some_and(|panel| {
            self.panel_column_rect(panel_id).is_some_and(|rect| {
                alignment_coverage_lane_clip(
                    &panel.pane_layout.annotation,
                    track_id,
                    f64::from(rect.left()),
                    f64::from(rect.width()),
                )
                .is_some()
            })
        })
    }

    /// A refinement that keeps the mode (a finer lane of the same authority) changes no label,
    /// so it needs a redraw but no display rebuild.
    fn redraw_on_alignment_coverage_refinement(&mut self) {
        let mut refined = false;
        for track in &mut self.alignment_tracks {
            if let Some(engine) = &track.coverage_engine {
                let sequence = engine.publication_sequence();
                refined |= std::mem::replace(&mut track.coverage_publication_sequence, sequence)
                    != sequence;
            }
        }
        if refined {
            self.window.request_redraw();
        }
    }

    /// States every panel's camera to every track's coverage engine and reconciles the mode the
    /// display was built from. Runs every poll: an unchanged request is a no-op in the engine.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn synchronize_alignment_coverage(&mut self) {
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let pyramid_pages_refused = self.alignment_depth_pyramid_pages_refused;
        let mut changed = false;
        for track_index in 0..self.alignment_tracks.len() {
            if self.alignment_tracks[track_index].coverage_engine.is_none() {
                continue;
            }
            let track_id = self.alignment_tracks[track_index].config.id;
            let ready = matches!(
                self.alignment_tracks[track_index].lifecycle,
                AlignmentTrackLifecycle::Ready
            );
            let filters = self.alignment_filters_for_track(track_id);
            let eligibility = alignment_coverage_eligibility(
                &filters,
                &self.alignment_tracks[track_index].active_filters,
            );
            for panel_id in &panel_ids {
                let panel_id = *panel_id;
                // Only a coverage lane that is on screen asks for coverage. A track scrolled out
                // of view, collapsed or hidden costs no decoding and never opens the worker's
                // own source handle, so descriptors and decode work follow visibility, not the
                // number of loaded tracks.
                let lane_on_screen = ready
                    && filters.show_coverage
                    && self.alignment_coverage_lane_on_screen(panel_id, track_id);
                let viewport = lane_on_screen
                    .then(|| self.alignment_coverage_viewport(panel_id, track_index))
                    .flatten()
                    .filter(|viewport| alignment_coverage_pyramid_zoom(viewport.bases_per_pixel()));
                let owner = alignment_coverage_owner(panel_id);
                let Some(viewport) = viewport else {
                    // Nothing is asked of the pyramid here: forget the owner's request and
                    // publication, and let the zoom alone name the lane's authority.
                    let track = &mut self.alignment_tracks[track_index];
                    let released = track
                        .projection_mut(panel_id)
                        .coverage
                        .request
                        .take()
                        .is_some();
                    if released && let Some(engine) = &track.coverage_engine {
                        engine.release(owner);
                    }
                    let coverage = &mut track.projection_mut(panel_id).coverage;
                    if coverage.presented.take().is_some() {
                        changed = true;
                    }
                    continue;
                };
                // Overview zoom never draws the exact lane, so it is never the authority there
                // even while an earlier scene is still retained.
                let exact_ready = viewport.detail_level() != DetailLevel::Overview
                    && exact_read_depth_ready(
                        true,
                        true,
                        self.alignment_exact_depth_interval(panel_id, track_id),
                        viewport.interval(),
                    );
                let track = &mut self.alignment_tracks[track_index];
                let coverage = &mut track.projection_mut(panel_id).coverage;
                let mut request = alignment_coverage_request(
                    coverage.request.as_ref(),
                    viewport.interval(),
                    viewport.bases_per_pixel(),
                    eligibility.eligibility.clone(),
                    exact_ready,
                );
                if coverage.request.is_none() {
                    request.generation = coverage.last_generation.saturating_add(1);
                }
                coverage.last_generation = request.generation;
                coverage.unapplied_view_predicates = eligibility.unapplied_view_predicates;
                coverage.request = Some(request.clone());
                let Some(engine) = &track.coverage_engine else {
                    continue;
                };
                engine.request(owner, request.clone());
                let summary = engine
                    .publication(owner)
                    .map(|publication| CoveragePublicationSummary::of(&publication, &request));
                let mode = alignment_coverage_mode_after_gpu_refusal(
                    select_alignment_coverage_mode(
                        request.bases_per_pixel,
                        exact_ready,
                        request.generation,
                        summary.as_ref(),
                    ),
                    pyramid_pages_refused,
                );
                let current = summary.filter(|summary| summary.generation == request.generation);
                let mate_overlap_not_collapsed = current
                    .as_ref()
                    .is_some_and(|summary| summary.mate_overlap_not_collapsed);
                let failure = current.and_then(|summary| summary.failure);
                let presented = Some((mode, failure));
                let coverage = &mut track.projection_mut(panel_id).coverage;
                if coverage.presented != presented
                    || coverage.mate_overlap_not_collapsed != mate_overlap_not_collapsed
                {
                    coverage.presented = presented;
                    coverage.mate_overlap_not_collapsed = mate_overlap_not_collapsed;
                    changed = true;
                }
            }
        }
        self.redraw_on_alignment_coverage_refinement();
        if changed {
            self.mark_display_dirty("alignment coverage authority changed");
        }
    }

    /// The mode the display of `(panel, track)` is built from.
    pub(crate) fn alignment_coverage_mode(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> AlignmentCoverageMode {
        let overview = self
            .panels
            .get(panel_id)
            .is_some_and(|panel| panel.viewport.detail_level() == DetailLevel::Overview);
        self.alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .and_then(|track| track.projection(panel_id))
            .and_then(|projection| projection.coverage.presented.as_ref())
            .map_or(
                if overview {
                    AlignmentCoverageMode::IndexDensity
                } else {
                    AlignmentCoverageMode::ReadEvidence
                },
                |(mode, _)| *mode,
            )
    }

    /// Relabels the coverage subtrack of an alignment annotation frame from the lane's current
    /// authority, and says at overview zoom why the other derived lanes are empty.
    pub(crate) fn label_alignment_coverage_subtrack(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        frame: &mut ui::TrackFrame,
    ) {
        let overview = self
            .panels
            .get(panel_id)
            .is_some_and(|panel| panel.viewport.detail_level() == DetailLevel::Overview);
        let coverage = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .and_then(|track| track.projection(panel_id))
            .map(|projection| &projection.coverage);
        let mode = self.alignment_coverage_mode(panel_id, track_id);
        let failure = coverage
            .and_then(|coverage| coverage.presented.as_ref())
            .and_then(|(_, failure)| failure.as_deref());
        let unapplied = coverage.map_or(0, |coverage| coverage.unapplied_view_predicates);
        let mate_overlap_not_collapsed =
            coverage.is_some_and(|coverage| coverage.mate_overlap_not_collapsed);
        let (label, detail) =
            alignment_coverage_lane_label(mode, failure, unapplied, mate_overlap_not_collapsed);
        for subtrack in &mut frame.subtracks {
            if subtrack.collapsed {
                continue;
            }
            match subtrack.alignment_subtrack {
                Some(ui::AlignmentSubtrack::Coverage) => {
                    subtrack.label.clone_from(&label);
                    subtrack.detail.clone_from(&detail);
                }
                Some(ui::AlignmentSubtrack::Sashimi) if overview => {
                    subtrack.detail = Some("Zoom in to load junctions".to_owned());
                }
                _ => {}
            }
        }
    }

    /// The pyramid lanes this frame draws: one per `(panel, track)` whose coverage lane is
    /// visible and whose mode is a pyramid mode, split around any exactly covered span.
    fn alignment_coverage_pyramid_draws(&self, scale_factor: f32) -> Vec<OwnedPyramidDraw> {
        let mut draws = Vec::<OwnedPyramidDraw>::new();
        for panel in self.panels.iter() {
            let Some(panel_rect) = self.panel_column_rect(panel.id) else {
                continue;
            };
            for (track_index, track) in self.alignment_tracks.iter().enumerate() {
                let track_id = track.config.id;
                let (Some(engine), Some(projection)) =
                    (&track.coverage_engine, track.projection(panel.id))
                else {
                    continue;
                };
                let Some(request) = &projection.coverage.request else {
                    continue;
                };
                let Some(clip) = alignment_coverage_lane_clip(
                    &panel.pane_layout.annotation,
                    track_id,
                    f64::from(panel_rect.left()),
                    f64::from(panel_rect.width()),
                ) else {
                    continue;
                };
                let Some(viewport) = self.alignment_coverage_viewport(panel.id, track_index) else {
                    continue;
                };
                let Some(publication) = engine.publication(alignment_coverage_owner(panel.id))
                else {
                    continue;
                };
                let overview = viewport.detail_level() == DetailLevel::Overview;
                let exact_interval = (!overview)
                    .then(|| self.alignment_exact_depth_interval(panel.id, track_id))
                    .flatten();
                let exact_ready =
                    exact_read_depth_ready(true, true, exact_interval, viewport.interval());
                let summary = CoveragePublicationSummary::of(&publication, request);
                if !select_alignment_coverage_mode(
                    viewport.bases_per_pixel(),
                    exact_ready,
                    request.generation,
                    Some(&summary),
                )
                .draws_pyramid()
                {
                    continue;
                }
                let Some(lane) = publication.lane.as_deref() else {
                    continue;
                };
                let Ok(camera) = CpuSummaryCamera::from_viewport(viewport, clip) else {
                    continue;
                };
                let filters = self.alignment_filters_for_track(track_id);
                let interval = viewport.interval();
                let scale_maximum = alignment_coverage_pyramid_scale_maximum(
                    filters.coverage_scale_maximum,
                    lane.maximum_depth(interval.start(), interval.end()),
                );
                let exact_clip = exact_interval
                    .and_then(|covered| alignment_scene_covered_clip(viewport, clip, covered));
                for part in alignment_coverage_pyramid_clips(clip, exact_clip) {
                    let Some(scissor) = alignment_clip_gpu_scissor(
                        part,
                        scale_factor,
                        self.surface_config.width,
                        self.surface_config.height,
                    ) else {
                        continue;
                    };
                    draws.push(OwnedPyramidDraw {
                        publication: Arc::clone(&publication),
                        camera,
                        scissor,
                        scale_maximum,
                        transform: filters.coverage_scale_transform,
                    });
                }
            }
        }
        draws
    }

    /// Captures this frame's pyramid lanes for the GPU. Called once per submitted frame, after
    /// the pane layout is final, so the lanes follow the live camera without a display rebuild.
    pub(crate) fn prepare_alignment_coverage_pyramid_frame(&mut self, scale_factor: f32) {
        let draws = self.alignment_coverage_pyramid_draws(scale_factor);
        if draws.is_empty() {
            self.alignment_depth_pyramid_gpu_frame = None;
            self.note_alignment_coverage_pyramid_pages_refused(0);
            return;
        }
        let scale = f64::from(scale_factor.max(f32::EPSILON));
        let screen_size_points = [
            (f64::from(self.surface_config.width) / scale) as f32,
            (f64::from(self.surface_config.height) / scale) as f32,
        ];
        let requests = draws
            .iter()
            .filter_map(|draw| {
                Some(AlignmentDepthPyramidGpuRequest {
                    lane: draw.publication.lane.as_deref()?,
                    camera: draw.camera,
                    screen_size_points,
                    scissor: draw.scissor,
                    scale_maximum: draw.scale_maximum,
                    transform: draw.transform,
                })
            })
            .collect::<Vec<_>>();
        self.alignment_depth_pyramid_gpu_frame = match self
            .alignment_depth_pyramid_gpu_renderer
            .prepare(&self.device, &self.queue, &requests)
        {
            Ok((frame, stats)) => {
                self.note_alignment_coverage_pyramid_pages_refused(stats.pages_refused);
                Some(frame)
            }
            Err(error) => {
                tracing::error!(%error, "could not prepare the alignment coverage pyramid");
                None
            }
        };
    }

    /// Records how many pyramid pages the GPU ledger refused this frame. A change is picked up
    /// by the next poll, which presents the affected lanes as pending (or complete again).
    fn note_alignment_coverage_pyramid_pages_refused(&mut self, pages_refused: usize) {
        if self.alignment_depth_pyramid_pages_refused != pages_refused {
            self.alignment_depth_pyramid_pages_refused = pages_refused;
            self.window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::{CoverageScaleTransform, OverlappingMatePolicy};

    fn interval(contig: u32, start: u64, end: u64) -> GenomicInterval {
        GenomicInterval::new(ContigId::new(contig), start, end).unwrap()
    }

    fn publication(generation: u64) -> CoveragePublicationSummary {
        CoveragePublicationSummary {
            generation,
            pyramid_authority: true,
            has_segments: true,
            exact_for_view: true,
            failure: None,
            mate_overlap_not_collapsed: false,
        }
    }

    #[test]
    fn mode_follows_zoom_evidence_and_publication_state() {
        use AlignmentCoverageMode::{
            IndexDensity, Pending, PyramidCoarse, PyramidExact, ReadEvidence,
        };
        let exact = publication(4);
        let coarse = CoveragePublicationSummary {
            exact_for_view: false,
            ..publication(4)
        };
        // (bases per pixel, exact read depth ready, publication, expected)
        let cases = [
            // Read and base zoom: never the pyramid, whatever is published.
            (0.5, true, Some(&exact), ReadEvidence),
            (9.9, false, Some(&exact), ReadEvidence),
            // Regional zoom: exact evidence wins; without it the pyramid stands in.
            (10.0, true, Some(&exact), ReadEvidence),
            (10.0, false, Some(&exact), PyramidExact),
            (250.0, false, Some(&coarse), PyramidCoarse),
            (250.0, false, None, Pending),
            // Overview zoom: the pyramid, or the index placeholder until it arrives.
            (1_000.0, false, None, IndexDensity),
            (1_000.0, false, Some(&coarse), PyramidCoarse),
            (50_000.0, false, Some(&exact), PyramidExact),
            // Overview never claims read evidence, even if a stale flag says so.
            (50_000.0, true, Some(&exact), PyramidExact),
        ];
        for (bases_per_pixel, ready, publication, expected) in cases {
            assert_eq!(
                select_alignment_coverage_mode(bases_per_pixel, ready, 4, publication),
                expected,
                "{bases_per_pixel} bp/px, ready {ready}"
            );
        }
    }

    #[test]
    fn publications_of_another_generation_are_ignored() {
        let stale = publication(3);
        assert_eq!(
            select_alignment_coverage_mode(5_000.0, false, 4, Some(&stale)),
            AlignmentCoverageMode::IndexDensity
        );
        assert_eq!(
            select_alignment_coverage_mode(100.0, false, 4, Some(&stale)),
            AlignmentCoverageMode::Pending
        );
        assert_eq!(
            select_alignment_coverage_mode(5_000.0, false, 3, Some(&stale)),
            AlignmentCoverageMode::PyramidExact
        );
    }

    #[test]
    fn a_stand_in_is_never_presented_as_complete() {
        for summary in [
            // A coarser ancestor, a gap, or an earlier camera's lane that does not show the
            // whole current view at its level.
            CoveragePublicationSummary {
                exact_for_view: false,
                ..publication(1)
            },
            // A unit the view needs failed; what is resident is still drawn.
            CoveragePublicationSummary {
                failure: Some(Arc::from("transfer budget exceeded")),
                ..publication(1)
            },
        ] {
            let mode = select_alignment_coverage_mode(2_000.0, false, 1, Some(&summary));
            assert_eq!(mode, AlignmentCoverageMode::PyramidCoarse);
            assert!(mode.is_pending());
            let (label, _) =
                alignment_coverage_lane_label(mode, summary.failure.as_deref(), 0, false);
            assert_eq!(label, "Coverage (loading…)");
        }
        // No segments, or read evidence as the authority: nothing of the pyramid to draw.
        for summary in [
            CoveragePublicationSummary {
                has_segments: false,
                ..publication(1)
            },
            CoveragePublicationSummary {
                pyramid_authority: false,
                ..publication(1)
            },
        ] {
            assert_eq!(
                select_alignment_coverage_mode(2_000.0, false, 1, Some(&summary)),
                AlignmentCoverageMode::IndexDensity
            );
        }
    }

    #[test]
    fn a_lane_is_exact_only_where_it_shows_the_whole_view_at_the_target_level() {
        let tiles = [(0, 4_096, 2), (4_096, 8_192, 2), (8_192, 12_288, 2)];
        // The view the lane was built for, and a pan across the same resident tiles.
        assert!(pyramid_segments_exact_over(tiles, 2, 1_000, 6_000));
        assert!(pyramid_segments_exact_over(tiles, 2, 5_000, 12_288));
        // Panned past what is resident.
        assert!(!pyramid_segments_exact_over(tiles, 2, 9_000, 13_000));
        // Zoomed to another level: the same tiles are now a stand-in.
        assert!(!pyramid_segments_exact_over(tiles, 1, 1_000, 6_000));
        assert!(!pyramid_segments_exact_over(tiles, 3, 1_000, 6_000));
        // A coarser ancestor in the middle, or a hole, is not exact.
        let mixed = [(0, 4_096, 2), (4_096, 8_192, 3), (8_192, 12_288, 2)];
        assert!(!pyramid_segments_exact_over(mixed, 2, 1_000, 9_000));
        assert!(pyramid_segments_exact_over(mixed, 2, 1_000, 4_096));
        let hole = [(0, 4_096, 2), (8_192, 12_288, 2)];
        assert!(!pyramid_segments_exact_over(hole, 2, 1_000, 9_000));
        assert!(!pyramid_segments_exact_over([], 2, 0, 10));
        // Segments wholly left of the view are skipped; an empty view needs nothing.
        assert!(pyramid_segments_exact_over(tiles, 2, 8_500, 9_000));
        assert!(pyramid_segments_exact_over([], 2, 10, 10));
    }

    #[test]
    fn a_lane_that_counts_overlapping_mates_twice_says_so() {
        use AlignmentCoverageMode::{
            IndexDensity, Pending, PyramidCoarse, PyramidExact, ReadEvidence,
        };
        // A complete pyramid lane names the caveat instead of reading as count-once coverage.
        let (label, detail) = alignment_coverage_lane_label(PyramidExact, None, 0, true);
        assert_eq!(label, "Coverage (overlapping mates counted twice)");
        assert_eq!(detail, None);
        assert_eq!(
            alignment_coverage_lane_label(PyramidExact, None, 0, false).0,
            "Coverage"
        );
        // The other details are kept.
        let (label, detail) = alignment_coverage_lane_label(PyramidExact, None, 2, true);
        assert_eq!(label, "Coverage (overlapping mates counted twice)");
        assert!(detail.unwrap().contains("2 view filter(s) not applied"));
        // A pending lane still reads as loading, whatever its tiles count.
        for mode in [PyramidCoarse, IndexDensity, Pending] {
            assert_eq!(
                alignment_coverage_lane_label(mode, None, 0, true).0,
                "Coverage (loading…)"
            );
        }
        // Read evidence is exact depth from loaded reads: the pyramid's caveat is not its own.
        assert_eq!(
            alignment_coverage_lane_label(ReadEvidence, None, 0, true).0,
            "Coverage"
        );
    }

    #[test]
    fn a_pyramid_the_gpu_could_not_wholly_draw_is_pending_not_complete() {
        use AlignmentCoverageMode::{
            IndexDensity, Pending, PyramidCoarse, PyramidExact, ReadEvidence,
        };
        assert_eq!(
            alignment_coverage_mode_after_gpu_refusal(PyramidExact, 0),
            PyramidExact
        );
        let refused = alignment_coverage_mode_after_gpu_refusal(PyramidExact, 2);
        assert_eq!(refused, PyramidCoarse);
        assert!(refused.is_pending() && refused.draws_pyramid());
        assert_eq!(
            alignment_coverage_lane_label(refused, None, 0, false).0,
            "Coverage (loading…)"
        );
        // Lanes that do not draw the pyramid, or are pending already, are unaffected.
        for mode in [ReadEvidence, PyramidCoarse, IndexDensity, Pending] {
            assert_eq!(alignment_coverage_mode_after_gpu_refusal(mode, 3), mode);
        }
    }

    #[test]
    fn labels_distinguish_loading_from_complete() {
        use AlignmentCoverageMode::{
            IndexDensity, Pending, PyramidCoarse, PyramidExact, ReadEvidence,
        };
        assert_eq!(
            alignment_coverage_lane_label(ReadEvidence, None, 3, false),
            ("Coverage".to_owned(), None)
        );
        assert_eq!(
            alignment_coverage_lane_label(PyramidExact, None, 0, false),
            ("Coverage".to_owned(), None)
        );
        let (label, detail) = alignment_coverage_lane_label(PyramidExact, None, 2, false);
        assert_eq!(label, "Coverage");
        assert!(detail.unwrap().starts_with("2 view filter(s) not applied"));
        let (label, detail) = alignment_coverage_lane_label(PyramidCoarse, None, 0, false);
        assert_eq!(label, "Coverage (loading…)");
        assert!(detail.unwrap().contains("refines"));
        let (label, detail) = alignment_coverage_lane_label(IndexDensity, None, 0, false);
        assert_eq!(label, "Coverage (loading…)");
        assert!(detail.unwrap().contains("Index-only overview"));
        assert_eq!(
            alignment_coverage_lane_label(Pending, None, 0, false).0,
            "Coverage (loading…)"
        );
        let (label, detail) =
            alignment_coverage_lane_label(PyramidCoarse, Some("no index"), 0, false);
        assert_eq!(label, "Coverage (loading…)");
        assert_eq!(detail.unwrap(), "Coverage unavailable: no index");
        for mode in [PyramidCoarse, IndexDensity, Pending] {
            assert!(mode.is_pending());
        }
        for mode in [ReadEvidence, PyramidExact] {
            assert!(!mode.is_pending());
        }
        assert_eq!(IndexDensity.automation_name(), "index-density");
        assert_eq!(PyramidCoarse.automation_name(), "pyramid-coarse");
        assert_eq!(PyramidExact.automation_name(), "pyramid-exact");
    }

    fn active(minimum_mapq: Option<u8>, secondary: bool, supplementary: bool) -> ActiveFilters {
        ActiveFilters {
            alignment: AlignmentFilter {
                minimum_mapping_quality: minimum_mapq,
                ..AlignmentFilter::default()
            },
            include_secondary: secondary,
            include_supplementary: supplementary,
        }
    }

    #[test]
    fn eligibility_follows_the_track_filter_settings() {
        let filters = SessionFilters::default();
        let base = alignment_coverage_eligibility(&filters, &active(None, true, true));
        assert_eq!(base.unapplied_view_predicates, 0);
        assert!(base.eligibility.include_secondary);
        assert!(base.eligibility.include_supplementary);
        assert_eq!(base.eligibility.filter.minimum_mapping_quality, None);
        assert_eq!(
            base.eligibility.overlapping_mate_policy,
            filters.overlapping_mate_policy
        );

        // MAPQ.
        let mapq = alignment_coverage_eligibility(&filters, &active(Some(30), true, true));
        assert_eq!(mapq.eligibility.filter.minimum_mapping_quality, Some(30));
        assert_ne!(mapq.eligibility, base.eligibility);

        // Secondary / supplementary inclusion.
        let primary_only = alignment_coverage_eligibility(&filters, &active(None, false, false));
        assert!(!primary_only.eligibility.include_secondary);
        assert!(!primary_only.eligibility.include_supplementary);
        assert_ne!(primary_only.eligibility, base.eligibility);

        // Duplicates and other SAM flags from the view filter.
        let mut flagged = SessionFilters::default();
        flagged.alignment_view_filter.all = vec![
            AlignmentViewPredicate::Flag {
                flag: AlignmentFlagPredicate::Duplicate,
                expected: false,
            },
            AlignmentViewPredicate::Flag {
                flag: AlignmentFlagPredicate::ProperPair,
                expected: true,
            },
            AlignmentViewPredicate::MappingQuality {
                minimum: Some(20),
                maximum: None,
            },
        ];
        let flags = alignment_coverage_eligibility(&flagged, &active(Some(5), true, true));
        assert_eq!(flags.unapplied_view_predicates, 0);
        assert_eq!(flags.eligibility.filter.excluded_flags, SAM_FLAG_DUPLICATE);
        assert_eq!(
            flags.eligibility.filter.required_flags,
            SAM_FLAG_PROPER_PAIR
        );
        assert_eq!(flags.eligibility.filter.minimum_mapping_quality, Some(20));
        assert_ne!(flags.eligibility, base.eligibility);

        // Overlapping-mate policy.
        let mut mates = SessionFilters::default();
        mates.overlapping_mate_policy = match mates.overlapping_mate_policy {
            OverlappingMatePolicy::CountOnce => OverlappingMatePolicy::CountBoth,
            OverlappingMatePolicy::CountBoth => OverlappingMatePolicy::CountOnce,
        };
        assert_ne!(
            alignment_coverage_eligibility(&mates, &active(None, true, true)).eligibility,
            base.eligibility
        );

        // A predicate the pyramid cannot evaluate is counted, never silently dropped.
        let mut template = SessionFilters::default();
        template.alignment_view_filter.all = vec![AlignmentViewPredicate::TemplateLength {
            minimum: Some(100),
            maximum: None,
        }];
        let unapplied = alignment_coverage_eligibility(&template, &active(None, true, true));
        assert_eq!(unapplied.unapplied_view_predicates, 1);
        assert_eq!(unapplied.eligibility, base.eligibility);
    }

    #[test]
    fn the_request_generation_advances_only_with_the_data_identity() {
        let filters = SessionFilters::default();
        let eligibility = alignment_coverage_eligibility(&filters, &active(None, true, true));
        let first = alignment_coverage_request(
            None,
            interval(2, 1_000, 9_000),
            12.5,
            eligibility.eligibility.clone(),
            false,
        );
        assert_eq!(first.generation, 1);
        assert_eq!(
            (first.contig, first.start, first.end),
            (ContigId::new(2), 1_000, 9_000)
        );
        assert!((first.bases_per_pixel - 12.5).abs() < f64::EPSILON);
        assert!(!first.exact_read_depth_ready);

        // A camera change keeps the generation and carries the new camera.
        let panned = alignment_coverage_request(
            Some(&first),
            interval(2, 5_000, 21_000),
            25.0,
            eligibility.eligibility.clone(),
            true,
        );
        assert_eq!(panned.generation, 1);
        assert_eq!((panned.start, panned.end), (5_000, 21_000));
        assert!(panned.exact_read_depth_ready);
        assert_ne!(panned, first);

        // Another contig or another filter is other data.
        let other_contig = alignment_coverage_request(
            Some(&panned),
            interval(3, 5_000, 21_000),
            25.0,
            eligibility.eligibility.clone(),
            false,
        );
        assert_eq!(other_contig.generation, 2);
        let filtered = alignment_coverage_eligibility(&filters, &active(Some(30), true, true));
        let other_filter = alignment_coverage_request(
            Some(&other_contig),
            interval(3, 5_000, 21_000),
            25.0,
            filtered.eligibility.clone(),
            false,
        );
        assert_eq!(other_filter.generation, 3);
        assert_eq!(other_filter.eligibility, filtered.eligibility);
    }

    #[test]
    fn exact_depth_is_ready_only_when_it_covers_the_whole_visible_interval() {
        let visible = interval(1, 1_000, 2_000);
        assert!(exact_read_depth_ready(
            true,
            true,
            Some(interval(1, 500, 2_500)),
            visible
        ));
        assert!(exact_read_depth_ready(true, true, Some(visible), visible));
        // Partly covered, another contig, stale contracts, or no scene at all.
        assert!(!exact_read_depth_ready(
            true,
            true,
            Some(interval(1, 1_200, 2_500)),
            visible
        ));
        assert!(!exact_read_depth_ready(
            true,
            true,
            Some(interval(2, 500, 2_500)),
            visible
        ));
        assert!(!exact_read_depth_ready(
            false,
            true,
            Some(interval(1, 500, 2_500)),
            visible
        ));
        assert!(!exact_read_depth_ready(
            true,
            false,
            Some(interval(1, 500, 2_500)),
            visible
        ));
        assert!(!exact_read_depth_ready(true, true, None, visible));
    }

    #[test]
    fn the_scale_uses_the_fixed_maximum_or_the_visible_maximum() {
        assert_eq!(alignment_coverage_pyramid_scale_maximum(Some(80), 400), 80);
        assert_eq!(alignment_coverage_pyramid_scale_maximum(None, 400), 400);
        assert_eq!(alignment_coverage_pyramid_scale_maximum(None, 0), 1);
        assert_eq!(alignment_coverage_pyramid_scale_maximum(Some(0), 9), 1);
        // The transform is the track's own setting, passed through unchanged.
        let filters = SessionFilters {
            coverage_scale_transform: CoverageScaleTransform::SquareRoot,
            ..SessionFilters::default()
        };
        assert_eq!(
            filters.coverage_scale_transform,
            CoverageScaleTransform::SquareRoot
        );
    }

    #[test]
    fn the_pyramid_is_drawn_only_where_exact_depth_is_not() {
        let clip = CpuClipRect {
            x: 100.0,
            y: 40.0,
            width: 800.0,
            height: 60.0,
        };
        assert_eq!(alignment_coverage_pyramid_clips(clip, None), vec![clip]);
        // Exact depth in the middle: the pyramid fills both sides.
        let middle = CpuClipRect {
            x: 300.0,
            width: 200.0,
            ..clip
        };
        let parts = alignment_coverage_pyramid_clips(clip, Some(middle));
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].x, parts[0].width), (100.0, 200.0));
        assert_eq!((parts[1].x, parts[1].width), (500.0, 400.0));
        assert!(parts.iter().all(|part| part.y.to_bits() == clip.y.to_bits()
            && part.height.to_bits() == clip.height.to_bits()));
        // Exact depth flush with the left edge: only the right side remains.
        let left = CpuClipRect {
            x: 100.0,
            width: 500.0,
            ..clip
        };
        let parts = alignment_coverage_pyramid_clips(clip, Some(left));
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].x, parts[0].width), (600.0, 300.0));
        // Exact depth over everything: nothing for the pyramid.
        assert!(alignment_coverage_pyramid_clips(clip, Some(clip)).is_empty());
    }

    fn annotation_layout(
        track_id: TrackId,
        show_coverage: bool,
        bases_per_pixel: u64,
        scroll_points: f32,
    ) -> ui::PaneLayoutSnapshot {
        let mut config = rgv_core::SessionAlignmentTrack::new(
            track_id,
            "sample".to_owned(),
            rgv_core::TrackSource::Local {
                path: "sample.bam".to_owned(),
            },
        );
        config.filters.show_coverage = show_coverage;
        let viewport = Viewport::new(interval(0, 0, 1_000 * bases_per_pixel), 1_000.0).unwrap();
        let frame = crate::display::alignment::alignment_scene_annotation_frame(
            &config, viewport, 50.0, None,
        );
        let row = egui::Rect::from_min_max(
            egui::pos2(0.0, frame.top),
            egui::pos2(1_000.0, frame.bottom),
        );
        let mut layout = ui::PaneLayoutSnapshot::empty(ui::TrackPane::Annotation);
        layout.scroll_points = scroll_points;
        layout.items.push(ui::PaneItemLayout {
            id: rgv_core::PresentationNodeId::Track {
                channel: rgv_core::TrackChannel::Annotation,
                track_id,
            },
            frame,
            row_rect: row,
            header_rect: row,
            content_rect: Some(row),
            // The pane shows 50..400 points.
            clip_rect: egui::Rect::from_min_max(egui::pos2(0.0, 50.0), egui::pos2(1_000.0, 400.0)),
            desired_content_visible: true,
            effective_content_visible: true,
        });
        layout
    }

    #[test]
    fn the_coverage_lane_exists_at_overview_zoom_and_is_clipped_to_the_pane() {
        let track = TrackId::new(7);
        // Overview (5,000 bp/px) and regional (100 bp/px) zoom expose the same coverage lane.
        for bases_per_pixel in [5_000, 100] {
            let layout = annotation_layout(track, true, bases_per_pixel, 0.0);
            let clip = alignment_coverage_lane_clip(&layout, track, 120.0, 900.0)
                .expect("an expanded coverage lane is drawable");
            assert_eq!((clip.x, clip.width), (120.0, 900.0));
            assert!(clip.y >= 50.0 && clip.y + clip.height <= 400.0);
            assert!(clip.height > 10.0, "{bases_per_pixel} bp/px: {clip:?}");
        }
        let layout = annotation_layout(track, true, 5_000, 0.0);
        // Another track, a zero-width panel, a hidden frame, a lane scrolled out of the pane.
        assert!(alignment_coverage_lane_clip(&layout, TrackId::new(8), 0.0, 900.0).is_none());
        assert!(alignment_coverage_lane_clip(&layout, track, 0.0, 0.0).is_none());
        let mut hidden = layout.clone();
        hidden.items[0].effective_content_visible = false;
        assert!(alignment_coverage_lane_clip(&hidden, track, 0.0, 900.0).is_none());
        let scrolled = annotation_layout(track, true, 5_000, 2_000.0);
        assert!(alignment_coverage_lane_clip(&scrolled, track, 0.0, 900.0).is_none());
        // A collapsed coverage lane (coverage switched off) draws nothing and asks nothing.
        let collapsed = annotation_layout(track, false, 5_000, 0.0);
        assert!(alignment_coverage_lane_clip(&collapsed, track, 0.0, 900.0).is_none());
    }

    #[test]
    fn a_loading_label_does_not_hide_the_coverage_lane_from_hit_testing() {
        let track = TrackId::new(7);
        let mut layout = annotation_layout(track, true, 100, 0.0);
        layout.pane_rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 50.0), egui::pos2(1_000.0, 400.0));
        let (top, bottom) = {
            let coverage = layout.items[0]
                .frame
                .subtracks
                .iter_mut()
                .find(|subtrack| {
                    subtrack.alignment_subtrack == Some(ui::AlignmentSubtrack::Coverage)
                })
                .unwrap();
            coverage.label =
                alignment_coverage_lane_label(AlignmentCoverageMode::PyramidCoarse, None, 0, false)
                    .0;
            (coverage.top, coverage.bottom)
        };
        let inside = (top + bottom) * 0.5;
        assert_eq!(
            crate::display::hit_test::alignment_lane_in_pane_layout(&layout, "Coverage", inside),
            Some(track)
        );
        assert_eq!(
            crate::display::hit_test::alignment_lane_in_pane_layout(&layout, "Sashimi", inside),
            None
        );
    }

    #[test]
    fn each_panel_is_its_own_coverage_owner() {
        assert_ne!(
            alignment_coverage_owner(PanelId::new(1)),
            alignment_coverage_owner(PanelId::new(2))
        );
        assert_eq!(
            alignment_coverage_owner(PanelId::new(7)),
            alignment_coverage_owner(PanelId::new(7))
        );
    }

    #[test]
    fn clearing_the_cache_removes_only_coverage_tiles() {
        let root = tempfile::tempdir().unwrap();
        let key = root.path().join("00112233445566778899aabbccddeeff");
        std::fs::create_dir_all(&key).unwrap();
        std::fs::write(key.join("c0-l3-t1.rgvcov"), [0_u8; 64]).unwrap();
        std::fs::write(key.join("c0-l3-t2.rgvcov"), [0_u8; 36]).unwrap();
        let foreign_directory = root.path().join("notes");
        std::fs::create_dir_all(&foreign_directory).unwrap();
        std::fs::write(foreign_directory.join("keep.txt"), b"keep").unwrap();
        std::fs::write(root.path().join("loose.rgvcov"), b"loose").unwrap();

        assert_eq!(clear_coverage_sidecar_directory(root.path()).unwrap(), 100);
        assert!(!key.exists(), "an emptied key directory is removed");
        assert!(foreign_directory.join("keep.txt").exists());
        assert!(root.path().join("loose.rgvcov").exists());
        assert_eq!(clear_coverage_sidecar_directory(root.path()).unwrap(), 0);
    }
}
