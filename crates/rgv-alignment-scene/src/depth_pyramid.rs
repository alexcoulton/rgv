//! The coverage-pyramid lane product: multi-resolution exact depth that is independent of read
//! evidence.
//!
//! Read-level depth ([`crate::ExactDepthSummaryTile`]) is derived from resident reads, so it
//! exists only where every read of the interval is held. A [`DepthPyramidStore`] holds
//! [`rgv_model::CoverageTile`]s instead, which a source builds at decode time without retaining
//! reads, so a view of any width has coverage.
//!
//! # Authority
//!
//! Where exact read-derived depth is resident and current for the visible interval it remains
//! the authority ([`DepthLaneAuthority::ExactReadEvidence`]). The pyramid serves when it is
//! not. Both count the same bases by the same rule, so where both exist a level-0 pyramid bin
//! equals the exact runs aggregated over the bin.
//!
//! # Memory
//!
//! Resident tiles are charged to the shared [`AllocationLedger`] as
//! [`AllocationClass::ExactSummary`], that is to the `Summary` owner, by exactly the bytes they
//! retain. The store keeps itself below its own byte limit (a fraction of the Summary share) by
//! evicting least recently used tiles, and gives way first when the Summary owner is over its
//! share: a pyramid tile can be reloaded from the sidecar cache without decoding, an exact
//! summary cannot.

use std::{collections::HashMap, mem::size_of, sync::Arc};

use rgv_core::ContigId;
use rgv_model::{
    CoverageBin, CoveragePyramidError, CoveragePyramidParameters, CoverageTile, CoverageTileId,
};

use crate::{
    AllocationClass, AllocationLease, AllocationLedger, AllocationOwner, AllocationOwnerUsage,
    LedgerError, ReservationKind,
};

/// Widest genomic span one decode unit produces: about 4 Mbp. A coarser tile is derived from
/// units of this size, so cancelling or interrupting whole-chromosome work loses at most one.
pub const DEPTH_PYRAMID_PRODUCTION_UNIT_BASES: u64 = 4 << 20;
/// Levels a coarse tile may sit above the tiles it is derived from (64 units per tile).
pub const DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH: u8 = 3;
/// Share of the Summary owner's budget the pyramid may hold, as a divisor.
pub const DEPTH_PYRAMID_SUMMARY_SHARE_DIVISOR: usize = 8;
/// Smallest useful store: every unit of one coarse tile, twice over.
pub const DEPTH_PYRAMID_MINIMUM_RESIDENT_BYTES: usize = 4 << 20;

/// Which product is the depth authority for a view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DepthLaneAuthority {
    /// Exact depth derived from resident, current read evidence.
    ExactReadEvidence,
    /// The coverage pyramid.
    Pyramid,
}

impl DepthLaneAuthority {
    /// Read evidence wins whenever it is resident and current for the whole visible interval.
    #[must_use]
    pub const fn select(exact_read_depth_ready: bool) -> Self {
        if exact_read_depth_ready {
            Self::ExactReadEvidence
        } else {
            Self::Pyramid
        }
    }
}

/// One resident pyramid tile with its exact ledger charge.
#[derive(Debug)]
pub struct DepthPyramidTile {
    tile: CoverageTile,
    allocation: AllocationLease,
}

impl DepthPyramidTile {
    /// Charges `tile` to the Summary owner and wraps it.
    pub fn register(
        tile: CoverageTile,
        ledger: &AllocationLedger,
        publication_revision: u64,
    ) -> Result<Arc<Self>, LedgerError> {
        let retained = size_of::<Self>().saturating_add(tile.owned_heap_bytes());
        let mut reservation = ledger.reserve(
            AllocationClass::ExactSummary,
            retained,
            ReservationKind::Normal,
        )?;
        let allocation = reservation.register(retained, publication_revision)?;
        Ok(Arc::new(Self { tile, allocation }))
    }

    #[must_use]
    pub const fn tile(&self) -> &CoverageTile {
        &self.tile
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }

    /// Bytes charged for this tile.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.tile.owned_heap_bytes())
    }
}

/// A camera's demand for coverage on one contig.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthPyramidView {
    pub contig: ContigId,
    pub contig_length: u64,
    /// Visible half-open base span.
    pub start: u64,
    pub end: u64,
    pub bases_per_pixel: f64,
}

impl DepthPyramidView {
    /// Visible span clipped to the contig.
    #[must_use]
    pub fn visible(&self) -> (u64, u64) {
        let end = self.end.min(self.contig_length);
        (self.start.min(end), end)
    }

    /// The visible span widened by half its width on each side, clipped to the contig.
    #[must_use]
    pub fn with_margin(&self) -> (u64, u64) {
        let (start, end) = self.visible();
        let margin = (end - start) / 2;
        (
            start.saturating_sub(margin),
            end.saturating_add(margin).min(self.contig_length),
        )
    }
}

/// What a view needs that the store does not hold.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepthPyramidPlan {
    /// The level whose bins best match a pixel of the view, capped at the contig's root level.
    pub target_level: u8,
    /// The level decode units are produced at; tiles above it are derived.
    pub production_level: u8,
    /// Missing production-level tiles under the visible span, left to right.
    pub missing_visible: Vec<CoverageTileId>,
    /// Missing production-level tiles under the margin only, nearest the view first.
    pub missing_margin: Vec<CoverageTileId>,
}

impl DepthPyramidPlan {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing_visible.is_empty() && self.missing_margin.is_empty()
    }

    /// The next unit to produce: visible first, then margin.
    #[must_use]
    pub fn next(&self) -> Option<CoverageTileId> {
        self.missing_visible
            .first()
            .or_else(|| self.missing_margin.first())
            .copied()
    }
}

/// A resident tile and the part of it a lane shows. Segments of one lane never overlap.
#[derive(Clone, Debug)]
pub struct DepthPyramidSegment {
    pub tile: Arc<DepthPyramidTile>,
    /// Half-open base span shown from this tile, aligned to its bins.
    pub start: u64,
    pub end: u64,
}

/// Depth statistics of one display column.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DepthPyramidColumn {
    pub mean_depth: f64,
    pub minimum_depth: u32,
    pub maximum_depth: u32,
    /// Bases the statistics describe; zero where no tile is resident yet.
    pub bases: u64,
}

/// Immutable snapshot of the best coverage resident for one view.
///
/// While the target level is incomplete the lane shows what there is: a coarser ancestor where
/// one is resident, finer tiles where those exist, nothing elsewhere. It is replaced as tiles
/// arrive.
#[derive(Clone, Debug)]
pub struct DepthPyramidLane {
    parameters: CoveragePyramidParameters,
    view: DepthPyramidView,
    target_level: u8,
    segments: Vec<DepthPyramidSegment>,
    visible_at_target: bool,
    visible_covered: bool,
}

impl DepthPyramidLane {
    #[must_use]
    pub const fn parameters(&self) -> CoveragePyramidParameters {
        self.parameters
    }

    #[must_use]
    pub const fn view(&self) -> &DepthPyramidView {
        &self.view
    }

    #[must_use]
    pub const fn target_level(&self) -> u8 {
        self.target_level
    }

    /// Disjoint segments in ascending genomic order.
    #[must_use]
    pub fn segments(&self) -> &[DepthPyramidSegment] {
        &self.segments
    }

    /// Every visible base is shown from a tile of the target level: nothing will refine.
    #[must_use]
    pub const fn is_exact_at_target(&self) -> bool {
        self.visible_at_target
    }

    /// Every visible base is shown from some level (possibly coarser or finer than the target).
    #[must_use]
    pub const fn covers_visible(&self) -> bool {
        self.visible_covered
    }

    /// Identity of what the lane shows, for change detection.
    #[must_use]
    pub fn signature(&self) -> Vec<(CoverageTileId, u64, u64)> {
        self.segments
            .iter()
            .map(|segment| (segment.tile.tile().id(), segment.start, segment.end))
            .collect()
    }

    /// Statistics of the bins intersecting `[start, end)`; `None` where nothing is resident.
    #[must_use]
    pub fn aggregate(&self, start: u64, end: u64) -> Option<(CoverageBin, u64)> {
        let mut total: Option<(CoverageBin, u64)> = None;
        let first = self
            .segments
            .partition_point(|segment| segment.end <= start);
        for segment in &self.segments[first..] {
            if segment.start >= end {
                break;
            }
            let Some((bin, bases)) = segment
                .tile
                .tile()
                .aggregate(start.max(segment.start), end.min(segment.end))
            else {
                continue;
            };
            total = Some(total.map_or((bin, bases), |(sum, covered)| {
                (sum.merge(bin), covered.saturating_add(bases))
            }));
        }
        total
    }

    /// Largest per-base depth of the bins intersecting `[start, end)`.
    #[must_use]
    pub fn maximum_depth(&self, start: u64, end: u64) -> u32 {
        self.aggregate(start, end)
            .map_or(0, |(bin, _)| bin.maximum_depth)
    }

    /// Projects the lane onto `columns` equal display columns over `[start, end)`. A column
    /// reports the merged statistics of every bin it touches, so a one-bin peak is the
    /// column's maximum however many bins the column spans.
    #[must_use]
    pub fn columns(&self, start: f64, end: f64, columns: usize) -> Vec<DepthPyramidColumn> {
        let mut output = vec![DepthPyramidColumn::default(); columns];
        if columns == 0 || !start.is_finite() || !end.is_finite() || start >= end {
            return output;
        }
        #[allow(clippy::cast_precision_loss)]
        let width = (end - start) / columns as f64;
        for (index, column) in output.iter_mut().enumerate() {
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let (column_start, column_end) = {
                let left = (start + width * index as f64).max(0.0);
                let right = (start + width * (index + 1) as f64).max(0.0);
                (
                    left.floor() as u64,
                    (right.ceil() as u64).max(left.floor() as u64 + 1),
                )
            };
            if let Some((bin, bases)) = self.aggregate(column_start, column_end) {
                *column = DepthPyramidColumn {
                    mean_depth: bin.mean_depth(bases),
                    minimum_depth: bin.minimum_depth,
                    maximum_depth: bin.maximum_depth,
                    bases,
                };
            }
        }
        output
    }
}

#[derive(Debug)]
struct Resident {
    tile: Arc<DepthPyramidTile>,
    last_used: u64,
}

/// Resident pyramid tiles of one source under one set of filter settings.
#[derive(Debug)]
pub struct DepthPyramidStore {
    parameters: CoveragePyramidParameters,
    ledger: AllocationLedger,
    tiles: HashMap<CoverageTileId, Resident>,
    clock: u64,
    resident_bytes: usize,
    byte_limit: usize,
    production_unit_bases: u64,
    evictions: u64,
}

impl DepthPyramidStore {
    /// `byte_limit` is the store's own cap; it is further limited to its fraction of the
    /// Summary owner's share whenever the ledger is consulted.
    #[must_use]
    pub fn new(
        parameters: CoveragePyramidParameters,
        ledger: AllocationLedger,
        byte_limit: usize,
    ) -> Self {
        Self {
            parameters,
            ledger,
            tiles: HashMap::new(),
            clock: 0,
            resident_bytes: 0,
            byte_limit: byte_limit.max(DEPTH_PYRAMID_MINIMUM_RESIDENT_BYTES),
            production_unit_bases: DEPTH_PYRAMID_PRODUCTION_UNIT_BASES,
            evictions: 0,
        }
    }

    /// Changes the widest span one decode unit covers (the default is
    /// [`DEPTH_PYRAMID_PRODUCTION_UNIT_BASES`]). Smaller units cancel and resume more finely
    /// at the cost of more index queries.
    #[must_use]
    pub const fn with_production_unit_bases(mut self, bases: u64) -> Self {
        self.production_unit_bases = bases;
        self
    }

    #[must_use]
    pub const fn parameters(&self) -> CoveragePyramidParameters {
        self.parameters
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Bytes charged to the ledger for the resident tiles; equal to the sum of their leases.
    #[must_use]
    pub const fn resident_bytes(&self) -> usize {
        self.resident_bytes
    }

    #[must_use]
    pub const fn evictions(&self) -> u64 {
        self.evictions
    }

    /// The cap in force: the configured limit, or the store's fraction of the Summary share
    /// when that is smaller, never below the minimum a coarse tile's derivation needs.
    #[must_use]
    pub fn effective_byte_limit(&self) -> usize {
        let share = self
            .ledger
            .owner_usage(AllocationOwner::Summary)
            .map_or(usize::MAX, |usage| {
                usage.soft_budget / DEPTH_PYRAMID_SUMMARY_SHARE_DIVISOR
            });
        self.byte_limit
            .min(share)
            .max(DEPTH_PYRAMID_MINIMUM_RESIDENT_BYTES)
    }

    #[must_use]
    pub fn contains(&self, id: CoverageTileId) -> bool {
        self.tiles.contains_key(&id)
    }

    /// The resident tile `id`, marked used.
    pub fn get(&mut self, id: CoverageTileId) -> Option<Arc<DepthPyramidTile>> {
        self.clock += 1;
        let clock = self.clock;
        self.tiles.get_mut(&id).map(|resident| {
            resident.last_used = clock;
            Arc::clone(&resident.tile)
        })
    }

    /// Makes `tile` resident (replacing any tile of the same id) and trims to the cap. The
    /// tile just inserted is never the one evicted.
    pub fn insert(
        &mut self,
        tile: CoverageTile,
    ) -> Result<Arc<DepthPyramidTile>, DepthPyramidError> {
        if tile.parameters() != self.parameters {
            return Err(CoveragePyramidError::IncompatibleChild(tile.id()).into());
        }
        self.clock += 1;
        let id = tile.id();
        let registered = DepthPyramidTile::register(tile, &self.ledger, self.clock)?;
        self.resident_bytes = self
            .resident_bytes
            .saturating_add(registered.estimated_bytes());
        if let Some(replaced) = self.tiles.insert(
            id,
            Resident {
                tile: Arc::clone(&registered),
                last_used: self.clock,
            },
        ) {
            self.resident_bytes = self
                .resident_bytes
                .saturating_sub(replaced.tile.estimated_bytes());
        }
        self.trim_protecting(Some(id));
        Ok(registered)
    }

    /// Evicts least recently used tiles down to the cap, and further by whatever the Summary
    /// owner is over its share. Returns the number evicted. Tiles a published lane still holds
    /// stay charged until that lane is dropped.
    pub fn trim(&mut self) -> usize {
        self.trim_protecting(None)
    }

    fn trim_protecting(&mut self, protected: Option<CoverageTileId>) -> usize {
        let overage = self
            .ledger
            .owner_usage(AllocationOwner::Summary)
            .map_or(0, AllocationOwnerUsage::overage);
        let goal = self
            .effective_byte_limit()
            .min(self.resident_bytes.saturating_sub(overage));
        if self.resident_bytes <= goal {
            return 0;
        }
        let mut order = self
            .tiles
            .iter()
            .filter(|(id, _)| Some(**id) != protected)
            .map(|(id, resident)| (resident.last_used, *id))
            .collect::<Vec<_>>();
        order.sort_unstable();
        let mut evicted = 0;
        for (_, id) in order {
            if self.resident_bytes <= goal {
                break;
            }
            if let Some(resident) = self.tiles.remove(&id) {
                self.resident_bytes = self
                    .resident_bytes
                    .saturating_sub(resident.tile.estimated_bytes());
                evicted += 1;
            }
        }
        self.evictions += evicted as u64;
        evicted
    }

    /// Drops every resident tile.
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.resident_bytes = 0;
    }

    /// The level a view is drawn from: bins of at least one pixel, capped at the contig root.
    #[must_use]
    pub fn target_level(&self, view: &DepthPyramidView) -> u8 {
        self.parameters
            .level_for_bases_per_pixel(view.bases_per_pixel)
            .min(self.parameters.root_level(view.contig_length))
    }

    /// The level decode units are produced at for a target level: the target itself while its
    /// tiles are no wider than the production unit, otherwise the coarsest
    /// such level, but never more than [`DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH`] below.
    #[must_use]
    pub fn production_level(&self, target_level: u8) -> u8 {
        let unit = (0..=target_level)
            .rev()
            .find(|level| {
                self.parameters
                    .tile_bases(*level)
                    .is_some_and(|bases| bases <= self.production_unit_bases)
            })
            .unwrap_or(0);
        unit.max(target_level.saturating_sub(DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH))
    }

    /// Whether `id` or an ancestor of it up to `ceiling` is resident.
    fn satisfied(&self, id: CoverageTileId, ceiling: u8) -> bool {
        let mut cursor = Some(id);
        while let Some(current) = cursor.filter(|current| current.level <= ceiling) {
            if self.tiles.contains_key(&current) {
                return true;
            }
            cursor = self.parameters.parent(current);
        }
        false
    }

    /// What `view` still needs. Does not mark anything used.
    #[must_use]
    pub fn plan(&self, view: &DepthPyramidView) -> DepthPyramidPlan {
        let target_level = self.target_level(view);
        let production_level = self.production_level(target_level);
        let (visible_start, visible_end) = view.visible();
        let (margin_start, margin_end) = view.with_margin();
        let missing = |start: u64, end: u64| {
            self.parameters
                .tile_range(production_level, start, end, view.contig_length)
                .map(|index| CoverageTileId::new(view.contig, production_level, index))
                .filter(|id| !self.satisfied(*id, target_level))
                .collect::<Vec<_>>()
        };
        let missing_visible = missing(visible_start, visible_end);
        let mut missing_margin = missing(margin_start, margin_end)
            .into_iter()
            .filter(|id| !missing_visible.contains(id))
            .collect::<Vec<_>>();
        // Nearest the view first, so panning in either direction is served evenly.
        let tile_bases = self.parameters.tile_bases(production_level).unwrap_or(1);
        let distance = |id: &CoverageTileId| {
            let start = id.index.saturating_mul(tile_bases);
            let end = start.saturating_add(tile_bases);
            if end <= visible_start {
                visible_start - end
            } else {
                start.saturating_sub(visible_end)
            }
        };
        missing_margin.sort_by_key(|id| (distance(id), id.index));
        DepthPyramidPlan {
            target_level,
            production_level,
            missing_visible,
            missing_margin,
        }
    }

    /// Makes `id` resident if it can be had without decoding: it already is, `load` supplies
    /// it (the sidecar cache), or all of its children can be had the same way within `depth`
    /// further levels. Every tile created on the way is appended to `created` (derived tiles
    /// are worth persisting; loaded ones are flagged `false`).
    pub fn obtain(
        &mut self,
        id: CoverageTileId,
        contig_length: u64,
        depth: u8,
        load: &mut dyn FnMut(CoverageTileId) -> Option<CoverageTile>,
        created: &mut Vec<(Arc<DepthPyramidTile>, bool)>,
    ) -> Result<Option<Arc<DepthPyramidTile>>, DepthPyramidError> {
        if let Some(tile) = self.get(id) {
            return Ok(Some(tile));
        }
        if let Some(tile) = load(id) {
            let tile = self.insert(tile)?;
            created.push((Arc::clone(&tile), false));
            return Ok(Some(tile));
        }
        if depth == 0 || id.level == 0 {
            return Ok(None);
        }
        let mut children = Vec::new();
        for child in self.parameters.children(id) {
            if self.parameters.tile_span(child, contig_length).is_none() {
                continue;
            }
            match self.obtain(child, contig_length, depth - 1, load, created)? {
                Some(tile) => children.push(tile),
                None => return Ok(None),
            }
        }
        let borrowed = children
            .iter()
            .map(|child| child.tile())
            .collect::<Vec<_>>();
        let derived = CoverageTile::from_children(id, self.parameters, contig_length, &borrowed)?;
        let tile = self.insert(derived)?;
        created.push((Arc::clone(&tile), true));
        Ok(Some(tile))
    }

    /// The best resident coverage for `view` over its visible span and margin.
    pub fn lane(&mut self, view: &DepthPyramidView) -> DepthPyramidLane {
        let target_level = self.target_level(view);
        let floor = target_level.saturating_sub(DEPTH_PYRAMID_MAXIMUM_DERIVATION_DEPTH);
        let root = self.parameters.root_level(view.contig_length);
        let (start, end) = view.with_margin();
        let mut segments = Vec::<DepthPyramidSegment>::new();
        let mut at_target = Vec::<(u64, u64)>::new();
        for index in self
            .parameters
            .tile_range(target_level, start, end, view.contig_length)
        {
            let id = CoverageTileId::new(view.contig, target_level, index);
            let before = segments.len();
            let mut gaps = Vec::new();
            self.cover(id, view.contig_length, floor, &mut segments, &mut gaps);
            if gaps.is_empty()
                && segments.len() == before + 1
                && segments[before].tile.tile().id() == id
            {
                at_target.push((segments[before].start, segments[before].end));
            }
            // Coarse first: where nothing at or below the target is resident, show the
            // nearest resident ancestor, clipped to the gap.
            if !gaps.is_empty() {
                let mut ancestor = self.parameters.parent(id);
                let mut fallback = None;
                while let Some(candidate) = ancestor.filter(|candidate| candidate.level <= root) {
                    if let Some(tile) = self.get(candidate) {
                        fallback = Some(tile);
                        break;
                    }
                    ancestor = self.parameters.parent(candidate);
                }
                if let Some(tile) = fallback {
                    for (gap_start, gap_end) in gaps {
                        segments.push(DepthPyramidSegment {
                            tile: Arc::clone(&tile),
                            start: gap_start,
                            end: gap_end,
                        });
                    }
                }
            }
        }
        segments.sort_by_key(|segment| segment.start);
        // Adjacent pieces of one tile are one segment.
        let mut merged = Vec::<DepthPyramidSegment>::with_capacity(segments.len());
        for segment in segments {
            if let Some(last) = merged.last_mut()
                && last.end == segment.start
                && Arc::ptr_eq(&last.tile, &segment.tile)
            {
                last.end = segment.end;
            } else {
                merged.push(segment);
            }
        }
        let (visible_start, visible_end) = view.visible();
        let covers = |spans: &mut dyn Iterator<Item = (u64, u64)>| {
            let mut cursor = visible_start;
            for (span_start, span_end) in spans {
                if span_start > cursor {
                    break;
                }
                cursor = cursor.max(span_end);
            }
            cursor >= visible_end
        };
        let visible_at_target = covers(&mut at_target.iter().copied());
        let visible_covered =
            covers(&mut merged.iter().map(|segment| (segment.start, segment.end)));
        DepthPyramidLane {
            parameters: self.parameters,
            view: *view,
            target_level,
            segments: merged,
            visible_at_target,
            visible_covered,
        }
    }

    /// Covers `id`'s span with `id` itself, else with resident descendants down to `floor`;
    /// spans nothing covers are appended to `gaps`.
    fn cover(
        &mut self,
        id: CoverageTileId,
        contig_length: u64,
        floor: u8,
        segments: &mut Vec<DepthPyramidSegment>,
        gaps: &mut Vec<(u64, u64)>,
    ) {
        let Some((start, end)) = self.parameters.tile_span(id, contig_length) else {
            return;
        };
        if let Some(tile) = self.get(id) {
            segments.push(DepthPyramidSegment { tile, start, end });
            return;
        }
        if id.level <= floor || id.level == 0 {
            gaps.push((start, end));
            return;
        }
        for child in self.parameters.children(id) {
            self.cover(child, contig_length, floor, segments, gaps);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DepthPyramidError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Pyramid(#[from] CoveragePyramidError),
}

#[cfg(test)]
mod tests;
