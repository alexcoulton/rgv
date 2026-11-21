//! Multi-resolution exact coverage: the coverage pyramid.
//!
//! A pyramid summarises the exact aligned-base depth of one contig at several resolutions.
//! Level 0 has bins of [`CoveragePyramidParameters::base_bin_bases`] bases; every further level
//! has bins [`CoveragePyramidParameters::level_factor`] times wider. Each bin stores the sum of
//! the per-base depth of the bases it covers, plus the smallest and largest per-base depth, so a
//! narrow peak or a dropout stays visible however far the view is zoomed out. Bins nest exactly:
//! a bin of level `n + 1` is the merge of `level_factor` consecutive bins of level `n`, so a
//! coarse level derived from a finer one is identical to the same level computed from the reads.
//!
//! Bins are paged in tiles of [`CoveragePyramidParameters::tile_bins`] bins. A tile is addressed
//! by contig, level and index and never depends on a camera.
//!
//! # Which bases count
//!
//! Depth is the exact aligned-base depth of the existing exact depth summary: a record
//! contributes one to every reference base under an `M`, `=` or `X` CIGAR operation; deletions
//! and reference skips contribute nothing. Which records are eligible is decided by
//! [`CoverageEligibility`], the same visibility and filter settings the read-level path uses.
//! With [`OverlappingMatePolicy::CountOnce`] a base covered by both mates of a template is
//! counted once. [`add_counted_depth_segments`] is the single implementation of that rule; the
//! alignment scene's exact depth summary and [`CoverageAccumulator`] both call it.

use std::collections::BTreeMap;

use rgv_core::{ContigId, OverlappingMatePolicy};
use thiserror::Error;

use crate::{
    AlignmentFilter, CigarOp, CigarOpKind, ExactDepthRun, QueryOptions,
    filter::FilterValue,
    pileup::{MateAlignment, OverlapCollapser},
};

/// Bases per bin at level 0.
///
/// Read-level display takes over below 10 bp per pixel, where exact depth runs are resident.
/// Sixteen bases is the first power of two past that threshold, so level 0 is at most 1.6
/// pixels wide where the pyramid is the only coverage authority, and one level-0 tile
/// (16,384 bp) is exactly four exact-summary tiles of 4,096 bp.
pub const COVERAGE_PYRAMID_BASE_BIN_BASES: u32 = 16;
/// Bin-width ratio between adjacent levels.
///
/// With a factor of four every zoom has a level whose bins are between one and four pixels
/// wide, all coarser levels together cost one third of the level they derive from, and a
/// 250 Mbp chromosome needs eight levels to fit one tile. A factor of two would halve the
/// widest bin on screen but double the levels to compute, store and page.
pub const COVERAGE_PYRAMID_LEVEL_FACTOR: u32 = 4;
/// Bins per tile: 16 KiB of bins, wide enough that one tile spans a typical window.
pub const COVERAGE_PYRAMID_TILE_BINS: u32 = 1_024;
/// Number of levels a pyramid may have. Level 11 has 67 Mbp bins and 68 Gbp tiles.
pub const COVERAGE_PYRAMID_MAX_LEVELS: u8 = 12;

/// Geometry of a pyramid. Tiles of different parameters are never interchangeable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CoveragePyramidParameters {
    base_bin_bases: u32,
    level_factor: u32,
    tile_bins: u32,
}

impl Default for CoveragePyramidParameters {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl CoveragePyramidParameters {
    pub const DEFAULT: Self = Self {
        base_bin_bases: COVERAGE_PYRAMID_BASE_BIN_BASES,
        level_factor: COVERAGE_PYRAMID_LEVEL_FACTOR,
        tile_bins: COVERAGE_PYRAMID_TILE_BINS,
    };

    /// `tile_bins` must be a multiple of `level_factor` so that a parent tile is exactly
    /// `level_factor` child tiles.
    pub fn new(
        base_bin_bases: u32,
        level_factor: u32,
        tile_bins: u32,
    ) -> Result<Self, CoveragePyramidError> {
        if base_bin_bases == 0
            || level_factor < 2
            || tile_bins == 0
            || !tile_bins.is_multiple_of(level_factor)
        {
            return Err(CoveragePyramidError::InvalidParameters);
        }
        Ok(Self {
            base_bin_bases,
            level_factor,
            tile_bins,
        })
    }

    #[must_use]
    pub const fn base_bin_bases(self) -> u32 {
        self.base_bin_bases
    }

    #[must_use]
    pub const fn level_factor(self) -> u32 {
        self.level_factor
    }

    #[must_use]
    pub const fn tile_bins(self) -> u32 {
        self.tile_bins
    }

    /// Bases per bin at `level`; `None` past [`COVERAGE_PYRAMID_MAX_LEVELS`] or on overflow.
    #[must_use]
    pub fn bin_bases(self, level: u8) -> Option<u64> {
        if level >= COVERAGE_PYRAMID_MAX_LEVELS {
            return None;
        }
        u64::from(self.level_factor)
            .checked_pow(u32::from(level))?
            .checked_mul(u64::from(self.base_bin_bases))
    }

    /// Bases per tile at `level`.
    #[must_use]
    pub fn tile_bases(self, level: u8) -> Option<u64> {
        self.bin_bases(level)?
            .checked_mul(u64::from(self.tile_bins))
    }

    /// Highest valid level.
    #[must_use]
    pub fn maximum_level(self) -> u8 {
        (0..COVERAGE_PYRAMID_MAX_LEVELS)
            .rev()
            .find(|level| self.tile_bases(*level).is_some())
            .unwrap_or(0)
    }

    /// The level whose bins best match one pixel: the finest level whose bins are at least one
    /// pixel wide. A pixel column then lies inside a single bin between one and
    /// `level_factor` pixels wide, so no bin can fall between two pixel centres and a peak or
    /// dropout a bin records is always drawn. Below the base bin width this is level 0.
    #[must_use]
    pub fn level_for_bases_per_pixel(self, bases_per_pixel: f64) -> u8 {
        let maximum = self.maximum_level();
        (0..=maximum)
            .find(|level| {
                self.bin_bases(*level).is_some_and(|bin_bases| {
                    #[allow(clippy::cast_precision_loss)]
                    let bin_bases = bin_bases as f64;
                    bin_bases >= bases_per_pixel
                })
            })
            .unwrap_or(maximum)
    }

    /// The first level at which a single tile holds a contig of `contig_length` bases.
    #[must_use]
    pub fn root_level(self, contig_length: u64) -> u8 {
        (0..=self.maximum_level())
            .find(|level| {
                self.tile_bases(*level)
                    .is_some_and(|bases| bases >= contig_length)
            })
            .unwrap_or_else(|| self.maximum_level())
    }

    /// Indices of the tiles of `level` that intersect `[start, end)`, clipped to the contig.
    #[must_use]
    pub fn tile_range(
        self,
        level: u8,
        start: u64,
        end: u64,
        contig_length: u64,
    ) -> std::ops::Range<u64> {
        let end = end.min(contig_length);
        let Some(tile_bases) = self.tile_bases(level) else {
            return 0..0;
        };
        if start >= end {
            return 0..0;
        }
        (start / tile_bases)..(end.saturating_sub(1) / tile_bases).saturating_add(1)
    }

    /// Half-open base span of a tile, clipped to the contig; `None` when it starts past the end.
    #[must_use]
    pub fn tile_span(self, id: CoverageTileId, contig_length: u64) -> Option<(u64, u64)> {
        let tile_bases = self.tile_bases(id.level)?;
        let start = id.index.checked_mul(tile_bases)?;
        if start >= contig_length {
            return None;
        }
        Some((start, start.saturating_add(tile_bases).min(contig_length)))
    }

    /// Number of bins a tile holds: fewer than `tile_bins` only for the contig's last tile.
    #[must_use]
    pub fn tile_bin_count(self, id: CoverageTileId, contig_length: u64) -> Option<usize> {
        let (start, end) = self.tile_span(id, contig_length)?;
        let bin_bases = self.bin_bases(id.level)?;
        usize::try_from((end - start).div_ceil(bin_bases)).ok()
    }

    /// The tile one level coarser that contains `id`.
    #[must_use]
    pub fn parent(self, id: CoverageTileId) -> Option<CoverageTileId> {
        let level = id.level.checked_add(1)?;
        self.tile_bases(level)?;
        Some(CoverageTileId {
            contig: id.contig,
            level,
            index: id.index / u64::from(self.level_factor),
        })
    }

    /// The tiles one level finer that `id` is the merge of, in order (not clipped to a contig).
    pub fn children(self, id: CoverageTileId) -> impl Iterator<Item = CoverageTileId> {
        let factor = u64::from(self.level_factor);
        let level = id.level.checked_sub(1);
        (0..factor).filter_map(move |offset| {
            Some(CoverageTileId {
                contig: id.contig,
                level: level?,
                index: id.index.checked_mul(factor)?.checked_add(offset)?,
            })
        })
    }

    /// Stable byte encoding for cache keys.
    #[must_use]
    pub fn canonical_bytes(self) -> [u8; 12] {
        let mut bytes = [0_u8; 12];
        bytes[..4].copy_from_slice(&self.base_bin_bases.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.level_factor.to_le_bytes());
        bytes[8..].copy_from_slice(&self.tile_bins.to_le_bytes());
        bytes
    }
}

/// Address of one tile of one contig's pyramid.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CoverageTileId {
    pub contig: ContigId,
    pub level: u8,
    pub index: u64,
}

impl CoverageTileId {
    #[must_use]
    pub const fn new(contig: ContigId, level: u8, index: u64) -> Self {
        Self {
            contig,
            level,
            index,
        }
    }
}

/// Exact depth statistics of the bases one bin covers.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct CoverageBin {
    /// Sum of the per-base depth over the bin's bases. Divide by the base count for the mean.
    pub depth_sum: u64,
    /// Smallest per-base depth in the bin.
    pub minimum_depth: u32,
    /// Largest per-base depth in the bin.
    pub maximum_depth: u32,
}

impl CoverageBin {
    /// Bytes of one bin in memory and in the sidecar.
    pub const ENCODED_BYTES: usize = 16;

    /// The identity of [`Self::merge`]: a bin that covers no base yet.
    const EMPTY: Self = Self {
        depth_sum: 0,
        minimum_depth: u32::MAX,
        maximum_depth: 0,
    };

    /// Statistics of the union of two disjoint base sets.
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            depth_sum: self.depth_sum.saturating_add(other.depth_sum),
            minimum_depth: if self.minimum_depth < other.minimum_depth {
                self.minimum_depth
            } else {
                other.minimum_depth
            },
            maximum_depth: if self.maximum_depth > other.maximum_depth {
                self.maximum_depth
            } else {
                other.maximum_depth
            },
        }
    }

    /// Mean per-base depth over `bases` bases.
    #[must_use]
    pub fn mean_depth(self, bases: u64) -> f64 {
        if bases == 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)]
        {
            self.depth_sum as f64 / bases as f64
        }
    }

    fn add_run(&mut self, bases: u64, depth: u32) {
        self.depth_sum = self
            .depth_sum
            .saturating_add(bases.saturating_mul(u64::from(depth)));
        self.minimum_depth = self.minimum_depth.min(depth);
        self.maximum_depth = self.maximum_depth.max(depth);
    }
}

/// One immutable page of a pyramid level.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverageTile {
    id: CoverageTileId,
    parameters: CoveragePyramidParameters,
    contig_length: u64,
    bins: Box<[CoverageBin]>,
}

impl CoverageTile {
    /// Wraps decoded bins. The bin count must be exactly what the tile's span requires, and
    /// every bin must be internally consistent (`min <= max`, `min * bases <= sum <= max *
    /// bases`), so a corrupt cache entry cannot become a tile.
    pub fn from_bins(
        id: CoverageTileId,
        parameters: CoveragePyramidParameters,
        contig_length: u64,
        bins: Vec<CoverageBin>,
    ) -> Result<Self, CoveragePyramidError> {
        let expected = parameters
            .tile_bin_count(id, contig_length)
            .ok_or(CoveragePyramidError::TileOutsideContig)?;
        if bins.len() != expected {
            return Err(CoveragePyramidError::BinCountMismatch {
                expected,
                actual: bins.len(),
            });
        }
        let tile = Self {
            id,
            parameters,
            contig_length,
            bins: bins.into_boxed_slice(),
        };
        for (index, bin) in tile.bins.iter().enumerate() {
            let bases = tile.bin_span(index).map_or(0, |(start, end)| end - start);
            let low = u64::from(bin.minimum_depth).saturating_mul(bases);
            let high = u64::from(bin.maximum_depth).saturating_mul(bases);
            if bin.minimum_depth > bin.maximum_depth || bin.depth_sum < low || bin.depth_sum > high
            {
                return Err(CoveragePyramidError::InconsistentBin);
            }
        }
        Ok(tile)
    }

    /// Aggregates exact constant-depth runs into the bins of one tile. Bases no run covers have
    /// depth zero. This is the definition the streaming accumulator must reproduce.
    pub fn from_depth_runs<'a>(
        id: CoverageTileId,
        parameters: CoveragePyramidParameters,
        contig_length: u64,
        runs: impl IntoIterator<Item = &'a ExactDepthRun>,
    ) -> Result<Self, CoveragePyramidError> {
        let (start, end) = parameters
            .tile_span(id, contig_length)
            .ok_or(CoveragePyramidError::TileOutsideContig)?;
        let bin_bases = parameters
            .bin_bases(id.level)
            .ok_or(CoveragePyramidError::InvalidLevel)?;
        let mut binner = RunBinner::new(start, end, bin_bases);
        let mut runs = runs
            .into_iter()
            .filter(|run| run.start < end && run.end > start && run.depth > 0)
            .collect::<Vec<_>>();
        runs.sort_unstable_by_key(|run| run.start);
        let mut cursor = start;
        for run in runs {
            let run_start = run.start.max(cursor);
            let run_end = run.end.min(end);
            if run_start >= run_end {
                continue;
            }
            binner.add(cursor, run_start, 0);
            binner.add(run_start, run_end, run.depth);
            cursor = run_end;
        }
        binner.add(cursor, end, 0);
        Self::from_bins(id, parameters, contig_length, binner.bins)
    }

    /// Derives a tile from the tiles one level finer. Every child that lies inside the contig
    /// must be supplied; the result equals the same tile computed from the reads.
    pub fn from_children(
        id: CoverageTileId,
        parameters: CoveragePyramidParameters,
        contig_length: u64,
        children: &[&Self],
    ) -> Result<Self, CoveragePyramidError> {
        let expected = parameters
            .tile_bin_count(id, contig_length)
            .ok_or(CoveragePyramidError::TileOutsideContig)?;
        let factor = parameters.level_factor as usize;
        let mut bins = vec![CoverageBin::EMPTY; expected];
        let mut covered = vec![false; expected];
        for (offset, child_id) in parameters.children(id).enumerate() {
            let Some(child_bins) = parameters.tile_bin_count(child_id, contig_length) else {
                continue;
            };
            let child = children
                .iter()
                .find(|child| child.id == child_id)
                .ok_or(CoveragePyramidError::MissingChild(child_id))?;
            if child.parameters != parameters
                || child.contig_length != contig_length
                || child.bins.len() != child_bins
            {
                return Err(CoveragePyramidError::IncompatibleChild(child_id));
            }
            let first = offset * (parameters.tile_bins as usize / factor);
            for (index, bin) in child.bins.iter().enumerate() {
                let target = first + index / factor;
                let slot = bins
                    .get_mut(target)
                    .ok_or(CoveragePyramidError::IncompatibleChild(child_id))?;
                *slot = slot.merge(*bin);
                covered[target] = true;
            }
        }
        if id.level == 0 || covered.iter().any(|covered| !covered) {
            return Err(CoveragePyramidError::MissingChild(id));
        }
        Self::from_bins(id, parameters, contig_length, bins)
    }

    #[must_use]
    pub const fn id(&self) -> CoverageTileId {
        self.id
    }

    #[must_use]
    pub const fn parameters(&self) -> CoveragePyramidParameters {
        self.parameters
    }

    #[must_use]
    pub const fn contig_length(&self) -> u64 {
        self.contig_length
    }

    #[must_use]
    pub fn bins(&self) -> &[CoverageBin] {
        &self.bins
    }

    /// Half-open base span of the tile, clipped to the contig.
    #[must_use]
    pub fn span(&self) -> (u64, u64) {
        self.parameters
            .tile_span(self.id, self.contig_length)
            .unwrap_or((0, 0))
    }

    /// Bases per bin (the last bin of a contig may cover fewer; see [`Self::bin_span`]).
    #[must_use]
    pub fn bin_bases(&self) -> u64 {
        self.parameters.bin_bases(self.id.level).unwrap_or(1)
    }

    /// Half-open base span of bin `index`, clipped to the contig.
    #[must_use]
    pub fn bin_span(&self, index: usize) -> Option<(u64, u64)> {
        if index >= self.bins.len() {
            return None;
        }
        let (start, end) = self.span();
        let bin_bases = self.bin_bases();
        let bin_start = start.saturating_add((index as u64).saturating_mul(bin_bases));
        Some((bin_start, bin_start.saturating_add(bin_bases).min(end)))
    }

    /// Largest per-base depth in the tile.
    #[must_use]
    pub fn maximum_depth(&self) -> u32 {
        self.bins
            .iter()
            .map(|bin| bin.maximum_depth)
            .max()
            .unwrap_or(0)
    }

    /// Statistics of the bins that intersect `[start, end)`, with the number of bases those
    /// bins cover. `None` when no bin intersects.
    #[must_use]
    pub fn aggregate(&self, start: u64, end: u64) -> Option<(CoverageBin, u64)> {
        let (tile_start, tile_end) = self.span();
        let start = start.max(tile_start);
        let end = end.min(tile_end);
        if start >= end {
            return None;
        }
        let bin_bases = self.bin_bases();
        let first = usize::try_from((start - tile_start) / bin_bases).ok()?;
        let last = usize::try_from((end - 1 - tile_start) / bin_bases).ok()?;
        let mut total = CoverageBin::EMPTY;
        let mut bases = 0_u64;
        for index in first..=last.min(self.bins.len().saturating_sub(1)) {
            total = total.merge(self.bins[index]);
            bases =
                bases.saturating_add(self.bin_span(index).map_or(0, |(start, end)| end - start));
        }
        Some((total, bases))
    }

    /// Heap bytes owned by the tile, excluding the inline value.
    #[must_use]
    pub fn owned_heap_bytes(&self) -> usize {
        self.bins.len().saturating_mul(size_of::<CoverageBin>())
    }

    /// Heap and inline bytes retained by the tile.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.owned_heap_bytes())
    }
}

/// Distributes constant-depth runs that tile `[start, end)` into bins.
struct RunBinner {
    start: u64,
    end: u64,
    bin_bases: u64,
    bins: Vec<CoverageBin>,
}

impl RunBinner {
    fn new(start: u64, end: u64, bin_bases: u64) -> Self {
        let count = usize::try_from(end.saturating_sub(start).div_ceil(bin_bases.max(1)))
            .unwrap_or(usize::MAX);
        Self {
            start,
            end,
            bin_bases: bin_bases.max(1),
            bins: vec![CoverageBin::EMPTY; count],
        }
    }

    fn add(&mut self, run_start: u64, run_end: u64, depth: u32) {
        let run_start = run_start.max(self.start);
        let run_end = run_end.min(self.end);
        if run_start >= run_end {
            return;
        }
        let mut cursor = run_start;
        while cursor < run_end {
            let offset = cursor - self.start;
            let index = usize::try_from(offset / self.bin_bases).unwrap_or(usize::MAX);
            let bin_end = self
                .start
                .saturating_add((offset / self.bin_bases + 1).saturating_mul(self.bin_bases))
                .min(run_end);
            let Some(bin) = self.bins.get_mut(index) else {
                return;
            };
            bin.add_run(bin_end - cursor, depth);
            cursor = bin_end;
        }
    }
}

/// Record visibility and filter settings that decide which records contribute coverage.
///
/// These are exactly the fields of [`QueryOptions`] that the shared eligibility rule and the
/// mate-overlap rule read; nothing else in a query's options can change exact depth.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageEligibility {
    pub include_secondary: bool,
    pub include_supplementary: bool,
    /// Flag requirements and exclusions (duplicates, QC failures), the mapping-quality
    /// threshold and auxiliary-field predicates.
    pub filter: AlignmentFilter,
    pub overlapping_mate_policy: OverlappingMatePolicy,
}

impl Default for CoverageEligibility {
    fn default() -> Self {
        Self::from_query_options(&QueryOptions::default())
    }
}

impl CoverageEligibility {
    #[must_use]
    pub fn from_query_options(options: &QueryOptions) -> Self {
        Self {
            include_secondary: options.include_secondary,
            include_supplementary: options.include_supplementary,
            filter: options.filter.clone(),
            overlapping_mate_policy: options.overlapping_mate_policy,
        }
    }

    /// Query options carrying these settings, for the shared eligibility rule.
    #[must_use]
    pub fn to_query_options(&self) -> QueryOptions {
        QueryOptions {
            include_secondary: self.include_secondary,
            include_supplementary: self.include_supplementary,
            filter: self.filter.clone(),
            overlapping_mate_policy: self.overlapping_mate_policy,
            ..QueryOptions::default()
        }
    }

    /// Unambiguous byte encoding of every setting, for a cache-key digest.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = b"rgv-coverage-eligibility-v1".to_vec();
        bytes.push(u8::from(self.include_secondary));
        bytes.push(u8::from(self.include_supplementary));
        bytes.push(match self.overlapping_mate_policy {
            OverlappingMatePolicy::CountOnce => 0,
            OverlappingMatePolicy::CountBoth => 1,
        });
        match self.filter.minimum_mapping_quality {
            None => bytes.extend_from_slice(&[0, 0]),
            Some(value) => bytes.extend_from_slice(&[1, value]),
        }
        bytes.extend_from_slice(&self.filter.required_flags.to_le_bytes());
        bytes.extend_from_slice(&self.filter.excluded_flags.to_le_bytes());
        bytes.extend_from_slice(&(self.filter.auxiliary.len() as u64).to_le_bytes());
        for predicate in &self.filter.auxiliary {
            bytes.extend_from_slice(&predicate.tag);
            bytes.push(predicate.comparison as u8);
            match &predicate.value {
                None => bytes.push(0),
                Some(FilterValue::Integer(value)) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                Some(FilterValue::Float(value)) => {
                    bytes.push(2);
                    bytes.extend_from_slice(&value.to_bits().to_le_bytes());
                }
                Some(FilterValue::String(value)) => {
                    bytes.push(3);
                    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
                    bytes.extend_from_slice(value.as_bytes());
                }
            }
        }
        bytes
    }
}

/// Adds the aligned-base depth one eligible record contributes inside `[start, end)`.
///
/// This is the count-once rule of exact depth, shared by the alignment scene's exact depth
/// summary and by the streaming coverage accumulator:
///
/// - only `M`, `=` and `X` operations contribute; deletions and reference skips do not;
/// - adjacent aligned operations are merged before they are counted;
/// - where the record's mate was seen earlier and contributed an aligned base, the base is not
///   counted again (per `collapser`'s policy); the earlier mate's deletions and skips leave the
///   later mate's bases counted.
///
/// `add(segment_start, segment_end)` receives each counted half-open segment, clipped to the
/// interval. `scratch` is reused between calls. The collapser is told how many bases were
/// collapsed. Records must be offered in the same order to every consumer that is expected to
/// agree; coordinate order is what the sources provide.
#[allow(clippy::too_many_arguments)]
pub fn add_counted_depth_segments(
    collapser: &mut OverlapCollapser,
    mate: MateAlignment<'_>,
    cigar: &[CigarOp],
    start: u64,
    end: u64,
    scratch: &mut Vec<(u64, u64)>,
    mut add: impl FnMut(u64, u64),
) {
    // Depth collapses only where the earlier mate has an aligned base; its deletions and `N`
    // skips leave the later mate's bases counted, as in the direct rgv-hts query.
    let overlap_mask = collapser.mask_for_alignment(mate, cigar.iter().copied(), |_| true);
    scratch.clear();
    let mut reference_position = mate.start;
    for operation in cigar {
        if reference_position >= end {
            // Nothing further along this read can reach the interval.
            break;
        }
        let operation_end = reference_position.saturating_add(u64::from(operation.len));
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                if operation_end <= start {
                    // Entirely left of the interval: contributes no depth here.
                } else if let Some((_, merged_end)) = scratch.last_mut()
                    && reference_position <= *merged_end
                {
                    *merged_end = (*merged_end).max(operation_end);
                } else {
                    scratch.push((reference_position, operation_end));
                }
                reference_position = operation_end;
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                reference_position = operation_end;
            }
            CigarOpKind::Insertion
            | CigarOpKind::SoftClip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {}
        }
    }
    let mut masked_bases = 0_u64;
    for &(aligned_start, aligned_end) in scratch.iter() {
        let clipped_start = aligned_start.max(start);
        let clipped_end = aligned_end.min(end);
        if clipped_start >= clipped_end {
            continue;
        }
        if let Some(mask) = &overlap_mask {
            masked_bases = masked_bases.saturating_add(mask.add_unmasked_depth_range(
                clipped_start,
                clipped_end,
                &mut add,
            ));
        } else {
            add(clipped_start, clipped_end);
        }
    }
    collapser.note_collapsed(masked_bases);
}

/// Tiles of one level over a run of consecutive tile indices, with their provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoverageAccumulation {
    pub tiles: Vec<CoverageTile>,
    /// Eligible records offered to the accumulator.
    pub eligible_records: u64,
    pub collapsed_mate_bases: u64,
    /// False when the count-once window overflowed, so some doubly covered bases may have been
    /// counted twice. Such tiles must not be presented or cached as exact.
    pub overlap_collapse_complete: bool,
    pub telemetry: CoverageAccumulatorTelemetry,
}

/// Working-set high-water marks of one accumulation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoverageAccumulatorTelemetry {
    /// Bins held (constant for the accumulation: interval / bin width).
    pub bins: usize,
    /// Most depth-change positions pending at once. Bounded by the distinct reference
    /// positions at which a read segment currently in flight starts or ends, never by depth.
    pub peak_pending_boundaries: usize,
    /// Most earlier mates remembered at once for the count-once rule.
    pub peak_pending_mates: usize,
    /// Largest CIGAR scratch capacity, in segments.
    pub peak_scratch_segments: usize,
}

impl CoverageAccumulatorTelemetry {
    /// Upper bound of the bytes the bins and pending boundaries retained at their peak.
    #[must_use]
    pub const fn peak_depth_state_bytes(self) -> usize {
        self.bins
            .saturating_mul(size_of::<CoverageBin>())
            .saturating_add(
                self.peak_pending_boundaries
                    .saturating_mul(PENDING_BOUNDARY_BYTES),
            )
            .saturating_add(
                self.peak_scratch_segments
                    .saturating_mul(size_of::<(u64, u64)>()),
            )
    }
}

/// Conservative bytes per pending depth-change position (key, delta and B-tree overhead).
const PENDING_BOUNDARY_BYTES: usize = 48;

/// Streaming builder of coverage tiles from coordinate-ordered eligible records.
///
/// It retains no read: per record it reads the placement, flags, mate placement, name (hashed
/// for the count-once rule) and CIGAR, and keeps only
///
/// - the bins of the requested tiles (interval / bin width),
/// - the reference positions at which depth will change ahead of the current record, and
/// - the count-once window of earlier mates, itself capped by the mate-overlap collapser.
///
/// None of these grows with depth: a thousand reads over the same bases share the same
/// boundary positions.
pub struct CoverageAccumulator {
    parameters: CoveragePyramidParameters,
    contig: ContigId,
    contig_length: u64,
    level: u8,
    first_tile: u64,
    binner: RunBinner,
    /// Net depth change at each pending reference position.
    pending: BTreeMap<u64, i64>,
    /// Every base before `cursor` has been binned.
    cursor: u64,
    depth: i64,
    collapser: OverlapCollapser,
    last_start: Option<u64>,
    eligible_records: u64,
    scratch: Vec<(u64, u64)>,
    segments: Vec<(u64, u64)>,
    telemetry: CoverageAccumulatorTelemetry,
}

impl std::fmt::Debug for CoverageAccumulator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CoverageAccumulator")
            .field("contig", &self.contig)
            .field("level", &self.level)
            .field("span", &self.span())
            .field("eligible_records", &self.eligible_records)
            .finish_non_exhaustive()
    }
}

impl CoverageAccumulator {
    /// Largest number of bins one accumulation may hold (64 MiB of bins).
    pub const MAXIMUM_BINS: u64 = 4 * 1_024 * 1_024;

    /// Accumulates the tiles `tiles` of `level` on `contig`.
    pub fn new(
        parameters: CoveragePyramidParameters,
        contig: ContigId,
        contig_length: u64,
        level: u8,
        tiles: std::ops::Range<u64>,
        overlapping_mate_policy: OverlappingMatePolicy,
    ) -> Result<Self, CoveragePyramidError> {
        let tile_bases = parameters
            .tile_bases(level)
            .ok_or(CoveragePyramidError::InvalidLevel)?;
        let bin_bases = parameters
            .bin_bases(level)
            .ok_or(CoveragePyramidError::InvalidLevel)?;
        if tiles.start >= tiles.end {
            return Err(CoveragePyramidError::TileOutsideContig);
        }
        let start = tiles
            .start
            .checked_mul(tile_bases)
            .filter(|start| *start < contig_length)
            .ok_or(CoveragePyramidError::TileOutsideContig)?;
        let end = tiles
            .end
            .checked_mul(tile_bases)
            .map_or(contig_length, |end| end.min(contig_length));
        if (end - start).div_ceil(bin_bases) > Self::MAXIMUM_BINS {
            return Err(CoveragePyramidError::TooManyBins);
        }
        let binner = RunBinner::new(start, end, bin_bases);
        let telemetry = CoverageAccumulatorTelemetry {
            bins: binner.bins.len(),
            ..CoverageAccumulatorTelemetry::default()
        };
        Ok(Self {
            parameters,
            contig,
            contig_length,
            level,
            first_tile: tiles.start,
            binner,
            pending: BTreeMap::new(),
            cursor: start,
            depth: 0,
            collapser: OverlapCollapser::new(overlapping_mate_policy),
            last_start: None,
            eligible_records: 0,
            scratch: Vec::new(),
            segments: Vec::new(),
            telemetry,
        })
    }

    /// Half-open base span the accumulator covers; the region whose records it must be given.
    #[must_use]
    pub const fn span(&self) -> (u64, u64) {
        (self.binner.start, self.binner.end)
    }

    #[must_use]
    pub const fn contig(&self) -> ContigId {
        self.contig
    }

    /// Adds one eligible record. Records must arrive in non-decreasing start order, which is
    /// what lets depth be finalised behind the current record.
    pub fn add_record(
        &mut self,
        mate: MateAlignment<'_>,
        cigar: &[CigarOp],
    ) -> Result<(), CoveragePyramidError> {
        if mate.contig != self.contig {
            return Err(CoveragePyramidError::WrongContig);
        }
        if self.last_start.is_some_and(|last| mate.start < last) {
            return Err(CoveragePyramidError::RecordsOutOfOrder);
        }
        self.last_start = Some(mate.start);
        self.eligible_records = self.eligible_records.saturating_add(1);
        self.flush_before(mate.start);

        let (start, end) = self.span();
        let mut segments = std::mem::take(&mut self.segments);
        segments.clear();
        add_counted_depth_segments(
            &mut self.collapser,
            mate,
            cigar,
            start,
            end,
            &mut self.scratch,
            |segment_start, segment_end| segments.push((segment_start, segment_end)),
        );
        for &(segment_start, segment_end) in &segments {
            *self.pending.entry(segment_start).or_insert(0) += 1;
            *self.pending.entry(segment_end).or_insert(0) -= 1;
        }
        self.telemetry.peak_scratch_segments = self
            .telemetry
            .peak_scratch_segments
            .max(self.scratch.capacity())
            .max(segments.capacity());
        self.segments = segments;
        self.telemetry.peak_pending_boundaries = self
            .telemetry
            .peak_pending_boundaries
            .max(self.pending.len());
        self.telemetry.peak_pending_mates = self
            .telemetry
            .peak_pending_mates
            .max(self.collapser.pending_len());
        Ok(())
    }

    /// Bins every base before `position`: no later record can change its depth.
    fn flush_before(&mut self, position: u64) {
        while let Some(entry) = self.pending.first_entry() {
            let boundary = *entry.key();
            if boundary >= position {
                break;
            }
            let delta = entry.remove();
            self.binner
                .add(self.cursor, boundary, nonnegative_u32(self.depth));
            self.cursor = self.cursor.max(boundary);
            self.depth = self.depth.saturating_add(delta);
        }
    }

    /// Finalises every remaining base and returns the tiles.
    pub fn finish(mut self) -> Result<CoverageAccumulation, CoveragePyramidError> {
        self.flush_before(u64::MAX);
        let (_, end) = self.span();
        self.binner
            .add(self.cursor, end, nonnegative_u32(self.depth));
        let tile_bins = self.parameters.tile_bins as usize;
        let mut tiles = Vec::new();
        let mut bins = std::mem::take(&mut self.binner.bins).into_iter();
        let mut index = self.first_tile;
        loop {
            let chunk = bins.by_ref().take(tile_bins).collect::<Vec<_>>();
            if chunk.is_empty() {
                break;
            }
            tiles.push(CoverageTile::from_bins(
                CoverageTileId::new(self.contig, self.level, index),
                self.parameters,
                self.contig_length,
                chunk,
            )?);
            index = index.saturating_add(1);
        }
        Ok(CoverageAccumulation {
            tiles,
            eligible_records: self.eligible_records,
            collapsed_mate_bases: self.collapser.collapsed_mate_bases(),
            overlap_collapse_complete: self.collapser.complete(),
            telemetry: self.telemetry,
        })
    }
}

fn nonnegative_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// Derives every tile of the levels above `tiles`' level that the given tiles fully determine.
///
/// `tiles` are tiles of one level. A parent is produced when all of its children that lie
/// inside the contig are present; the result is repeated upwards as far as the contig's root
/// level (the first level at which one tile holds the contig). The output is ordered by level,
/// then index, and does not include the input.
pub fn derive_coarser_tiles(tiles: &[CoverageTile]) -> Vec<CoverageTile> {
    let mut output = Vec::new();
    let mut source = tiles.to_vec();
    while let Some(first) = source.first() {
        let parameters = first.parameters;
        let contig_length = first.contig_length;
        if first.id.level >= parameters.root_level(contig_length) {
            // One tile already holds the contig; coarser levels add nothing.
            break;
        }
        let mut parents = source
            .iter()
            .filter_map(|tile| parameters.parent(tile.id))
            .collect::<Vec<_>>();
        parents.sort_unstable();
        parents.dedup();
        let children = source.iter().collect::<Vec<_>>();
        let next = parents
            .into_iter()
            .filter_map(|parent| {
                CoverageTile::from_children(parent, parameters, contig_length, &children).ok()
            })
            .collect::<Vec<_>>();
        output.extend(next.iter().cloned());
        source = next;
    }
    output
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CoveragePyramidError {
    #[error("coverage pyramid parameters are invalid")]
    InvalidParameters,
    #[error("coverage pyramid level is out of range")]
    InvalidLevel,
    #[error("coverage tile lies outside its contig")]
    TileOutsideContig,
    #[error("coverage tile holds {actual} bins but its span requires {expected}")]
    BinCountMismatch { expected: usize, actual: usize },
    #[error("coverage bin statistics are inconsistent")]
    InconsistentBin,
    #[error("coverage tile {0:?} is needed but absent")]
    MissingChild(CoverageTileId),
    #[error("coverage tile {0:?} does not belong to this pyramid")]
    IncompatibleChild(CoverageTileId),
    #[error("record belongs to another contig")]
    WrongContig,
    #[error("records were not offered in coordinate order")]
    RecordsOutOfOrder,
    #[error("coverage request needs too many bins for one accumulation")]
    TooManyBins,
}

#[cfg(test)]
mod tests;
