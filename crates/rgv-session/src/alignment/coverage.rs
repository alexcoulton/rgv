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

use rgv_alignment_scene::{CpuClipRect, DepthLaneAuthority};
use rgv_core::{
    AlignmentAuxComparison, AlignmentAuxFilterValue, AlignmentFlagPredicate,
    AlignmentViewPredicate, DetailLevel, GenomicInterval, PanelId, SessionFilters,
};
use rgv_engine::{
    AlignmentCoverageEngine, AlignmentCoverageEngineConfig, AlignmentCoveragePublication,
    AlignmentCoverageRequest, AlignmentCoverageState, SourceOwnerId,
};
use rgv_hts::{
    AlignmentFilter, AlignmentOpenOptions, AlignmentSourceStamp, AuxComparison, AuxPredicate,
    CoverageEligibility, CoverageSidecarCache, FilterValue, QueryOptions,
};

use crate::alignment::track::ActiveFilters;

const SAM_FLAG_PAIRED: u16 = 0x1;
const SAM_FLAG_PROPER_PAIR: u16 = 0x2;
const SAM_FLAG_REVERSE: u16 = 0x10;
const SAM_FLAG_SECONDARY: u16 = 0x100;
const SAM_FLAG_FAILED_QC: u16 = 0x200;
const SAM_FLAG_DUPLICATE: u16 = 0x400;
const SAM_FLAG_SUPPLEMENTARY: u16 = 0x800;

/// What the coverage lane of one `(panel, track)` is drawn from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentCoverageMode {
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
    pub const fn draws_pyramid(self) -> bool {
        matches!(self, Self::PyramidExact | Self::PyramidCoarse)
    }

    /// Whether the lane must be presented as still loading.
    pub const fn is_pending(self) -> bool {
        matches!(
            self,
            Self::PyramidCoarse | Self::IndexDensity | Self::Pending
        )
    }

    /// Stable name reported by the automation workspace snapshot.
    pub const fn automation_name(self) -> &'static str {
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
pub struct CoveragePublicationSummary {
    pub generation: u64,
    /// The pyramid, not read evidence, is the publication's authority.
    pub pyramid_authority: bool,
    /// The lane has at least one resident segment to draw.
    pub has_segments: bool,
    /// Every base the panel shows now is drawn from a tile of the level its zoom asks for.
    /// Judged against the current camera, not the one the lane was built for, so panning
    /// across resident tiles stays exact while a zoom to another level does not.
    pub exact_for_view: bool,
    pub failure: Option<Arc<str>>,
    /// Part of the visible coverage counts overlapping mates twice although the request asked
    /// for them to be counted once (the count-once window overflowed).
    pub mate_overlap_not_collapsed: bool,
}

impl CoveragePublicationSummary {
    /// Summarises `publication` against the request the panel currently states.
    pub fn of(
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
pub fn pyramid_segments_exact_over(
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
pub fn alignment_coverage_pyramid_zoom(bases_per_pixel: f64) -> bool {
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
pub fn select_alignment_coverage_mode(
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
pub const fn alignment_coverage_mode_after_gpu_refusal(
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
pub fn alignment_coverage_lane_label(
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
pub struct AlignmentCoverageEligibility {
    pub eligibility: CoverageEligibility,
    /// View predicates the pyramid cannot evaluate from a streamed record (template length,
    /// CIGAR events, substitutions, …). The lane says so while the pyramid is shown.
    pub unapplied_view_predicates: usize,
}

/// Derives the pyramid's eligibility from the same settings the read path filters by: MAPQ,
/// secondary/supplementary inclusion, SAM-flag predicates (duplicates, failed QC, pairing,
/// strand), auxiliary-tag predicates and the overlapping-mate policy. A change to any of them
/// changes the result, and so the pyramid.
pub fn alignment_coverage_eligibility(
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
pub fn alignment_coverage_request(
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
pub fn exact_read_depth_ready(
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
pub fn alignment_coverage_pyramid_scale_maximum(
    fixed_maximum: Option<u32>,
    visible_maximum_depth: u32,
) -> u32 {
    fixed_maximum.unwrap_or(visible_maximum_depth).max(1)
}

/// The parts of `clip` the exact lane does not draw: all of it without exact coverage, else
/// what lies left and right of the exactly covered span. The pyramid is drawn only there, so
/// the two authorities never overlap.
pub fn alignment_coverage_pyramid_clips(
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
pub struct AlignmentCoverageProjection {
    /// The request last stated to the engine; `None` while the panel asks for nothing.
    pub request: Option<AlignmentCoverageRequest>,
    /// Last generation used, kept across releases so generations never repeat for an owner.
    pub last_generation: u64,
    pub unapplied_view_predicates: usize,
    /// Mode and failure the display was last built from; a change rebuilds the display.
    pub presented: Option<(AlignmentCoverageMode, Option<Arc<str>>)>,
    /// Whether the presented pyramid counts overlapping mates twice; part of the lane label.
    pub mate_overlap_not_collapsed: bool,
}

/// One coverage owner per panel within a track's engine.
pub const fn alignment_coverage_owner(panel_id: PanelId) -> SourceOwnerId {
    SourceOwnerId::new(panel_id.get())
}

/// The stamp a source was opened at, recovered from the scene engine's source revision (the
/// revision is built from the stamp bit for bit by `AlignmentSourceRevision::from`).
pub fn alignment_source_stamp_of(
    revision: rgv_alignment_scene::AlignmentSourceRevision,
) -> AlignmentSourceStamp {
    let bytes = |fingerprint: rgv_alignment_scene::RevisionFingerprint| {
        let mut bytes = [0_u8; 16];
        bytes[..8].copy_from_slice(&fingerprint.high().to_le_bytes());
        bytes[8..].copy_from_slice(&fingerprint.low().to_le_bytes());
        bytes
    };
    AlignmentSourceStamp::new(
        bytes(revision.source().fingerprint()),
        bytes(revision.revision()),
    )
}

/// Spawns the coverage worker of one alignment source. `None` when the thread cannot be
/// started; the track then simply has no pyramid. `known` is what the track's scene engine
/// reported when it opened the same source: with it the worker opens the source only to
/// decode, so a request the sidecar cache serves opens no file.
pub fn spawn_alignment_coverage_engine(
    path: std::path::PathBuf,
    open_options: AlignmentOpenOptions,
    ledger: rgv_alignment_scene::AllocationLedger,
    sidecar: Option<Arc<CoverageSidecarCache>>,
    known: Option<&rgv_engine::AlignmentSceneSourceMetadata>,
) -> Option<AlignmentCoverageEngine> {
    let mut config = AlignmentCoverageEngineConfig::new(path, open_options, ledger);
    if let Some(metadata) = known {
        config = config.with_known_source(
            metadata.dictionary.clone(),
            Some(alignment_source_stamp_of(metadata.source_revision)),
        );
    }
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
pub fn retire_alignment_coverage_engine(
    engine: AlignmentCoverageEngine,
) -> Option<std::thread::JoinHandle<()>> {
    engine.retire()
}

/// Deletes every coverage tile under `root` through the cache's own [`CoverageSidecarCache::clear`]:
/// only tile files inside the cache's key directories, then the directories that became empty.
/// Returns the bytes removed.
pub fn clear_coverage_sidecar_directory(root: &std::path::Path) -> std::io::Result<u64> {
    CoverageSidecarCache::open(root, rgv_hts::COVERAGE_SIDECAR_DEFAULT_MAXIMUM_BYTES)?.clear()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::{ContigId, CoverageScaleTransform, OverlappingMatePolicy};

    #[test]
    fn the_source_stamp_is_recovered_bit_for_bit_from_the_scene_revision() {
        let stamp = AlignmentSourceStamp::new(
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            [
                16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
            ],
        );
        let revision = rgv_alignment_scene::AlignmentSourceRevision::from(stamp);
        assert_eq!(alignment_source_stamp_of(revision), stamp);
    }

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
