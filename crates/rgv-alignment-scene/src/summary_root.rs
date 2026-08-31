use std::{mem::size_of, sync::Arc};

use rgv_core::GenomicInterval;
use rgv_hts::{JunctionEvidence, JunctionProvenance, ModificationSummary, PairSummary, PileupBin};
use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AlleleSummaryDetail, AllocationClass, AllocationLease,
    AllocationLedger, ContractFingerprint, EvidenceRevision, ExactAlleleSummaryKey,
    ExactAlleleSummaryTile, ExactDepthSummaryKey, ExactDepthSummaryTile, ExactFilterSummaryKey,
    ExactFilterSummaryTile, ExactJunctionSummaryKey, ExactJunctionSummaryTile,
    ExactModificationSummaryKey, ExactModificationSummaryTile, ExactPairSummaryKey,
    ExactPairSummaryTile, FilteredRecordProvenance, LedgerError, PinnedAllocationLease,
    ReservationKind, SummaryContract, SummaryRevision, pair_summary::summarize_pair_candidate_iter,
};

const EXACT_SUMMARY_OVERVIEW_BINS: usize = 256;

/// One immutable exact-summary authority graph for a prepared canonical genomic horizon.
#[derive(Debug)]
pub struct ExactSummaryRoot {
    revision: SummaryRevision,
    evidence_revision: EvidenceRevision,
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    contract: SummaryContract,
    prepared: GenomicInterval,
    depth: Arc<[Arc<ExactDepthSummaryTile>]>,
    alleles: Arc<[Arc<ExactAlleleSummaryTile>]>,
    junctions: Arc<[Arc<ExactJunctionSummaryTile>]>,
    expanded_junction_rows: Arc<[ExpandedJunctionRowAssignment]>,
    pairs: Arc<[Arc<ExactPairSummaryTile>]>,
    filters: Arc<[Arc<ExactFilterSummaryTile>]>,
    modifications: Arc<[Arc<ExactModificationSummaryTile>]>,
    /// Exact paired-template statistics for the immutable prepared horizon. Settings and other
    /// frame-shell consumers read this O(1) derivative instead of rescanning every pair candidate
    /// on every redraw.
    prepared_pair_summary: PairSummary,
    /// Fixed-size, exact depth projection built with the immutable root on the worker. UI frames
    /// consume this bounded derivative instead of rescanning every retained depth run.
    overview_depths: Arc<[u32]>,
    metadata_allocation: AllocationLease,
}

/// Camera-independent expanded-row ownership for one exact biological junction. Assignments are
/// derived once from the complete prepared summary root, so panning never repacks visible arcs
/// or uploads replacement geometry. A wider immutable root may choose a different assignment,
/// but that change is published atomically with the successor scene.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExpandedJunctionRowAssignment {
    start: u64,
    end: u64,
    row: u32,
}

/// Exact allele evidence owned by the canonical summary tile containing one genomic position.
///
/// This is deliberately independent of a viewport query and of GPU residency. Consumers can
/// inspect the same immutable counts that produced the published allele page while retaining the
/// tile-local evidence provenance needed to describe completeness truthfully.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactAlleleSiteEvidence {
    pub owner_interval: GenomicInterval,
    pub bin: PileupBin,
    pub eligible_records: usize,
    pub collapsed_mate_bases: u64,
}

/// Exact single-base pileup evidence from the immutable depth authority, optionally enriched by
/// the independently retained allele family. Depth remains available when sequence detail was not
/// requested, so coverage inspection never depends on an allele render primitive existing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactPileupSiteEvidence {
    pub owner_interval: GenomicInterval,
    pub bin: PileupBin,
    pub eligible_records: usize,
    pub collapsed_mate_bases: u64,
    pub allele_observations_available: bool,
}

impl ExactSummaryRoot {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn build(
        revision: SummaryRevision,
        evidence_revision: EvidenceRevision,
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        contract: SummaryContract,
        target: GenomicInterval,
        mut depth: Vec<Arc<ExactDepthSummaryTile>>,
        mut alleles: Vec<Arc<ExactAlleleSummaryTile>>,
        mut junctions: Vec<Arc<ExactJunctionSummaryTile>>,
        mut pairs: Vec<Arc<ExactPairSummaryTile>>,
        mut filters: Vec<Arc<ExactFilterSummaryTile>>,
        mut modifications: Vec<Arc<ExactModificationSummaryTile>>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactSummaryRootError> {
        depth.sort_by_key(|tile| tile.tile());
        alleles.sort_by_key(|tile| tile.tile());
        junctions.sort_by_key(|tile| tile.tile());
        pairs.sort_by_key(|tile| tile.tile());
        filters.sort_by_key(|tile| tile.tile());
        modifications.sort_by_key(|tile| tile.tile());
        if depth.is_empty() {
            return Err(ExactSummaryRootError::MissingDepthTiles);
        }
        let prepared = validate_contiguous_depth(&depth)?;
        if !prepared.contains_interval(target) {
            return Err(ExactSummaryRootError::TargetOutsidePreparedInterval);
        }
        let expected_tiles = depth.iter().map(|tile| tile.tile()).collect::<Vec<_>>();
        validate_family_tiles(
            &expected_tiles,
            &alleles,
            contract.allele_detail != AlleleSummaryDetail::None,
            |tile| tile.tile(),
        )?;
        validate_family_tiles(&expected_tiles, &junctions, true, |tile| tile.tile())?;
        validate_family_tiles(&expected_tiles, &pairs, true, |tile| tile.tile())?;
        validate_family_tiles(&expected_tiles, &filters, true, |tile| tile.tile())?;
        validate_family_tiles(
            &expected_tiles,
            &modifications,
            contract.collect_modifications,
            |tile| tile.tile(),
        )?;

        for tile in &depth {
            let expected = ExactDepthSummaryKey::new(
                source_revision,
                record_contract,
                contract.depth(),
                tile.tile(),
            );
            if tile.key() != expected {
                return Err(ExactSummaryRootError::ContractMismatch);
            }
        }
        for tile in &alleles {
            let expected = ExactAlleleSummaryKey::new(
                source_revision,
                record_contract,
                contract.alleles(),
                tile.tile(),
            );
            if tile.key() != expected {
                return Err(ExactSummaryRootError::ContractMismatch);
            }
        }
        for tile in &junctions {
            let expected = ExactJunctionSummaryKey::new(
                source_revision,
                record_contract,
                contract.junctions(),
                tile.summary().provenance.normalization_mapped_records,
                tile.tile(),
            );
            if tile.key() != expected {
                return Err(ExactSummaryRootError::ContractMismatch);
            }
        }
        let junction_normalization = junctions
            .first()
            .and_then(|tile| tile.summary().provenance.normalization_mapped_records);
        if junctions.iter().any(|tile| {
            tile.summary().provenance.normalization_mapped_records != junction_normalization
        }) {
            // Raw counts and CPM are different semantic authorities. A root with per-tile
            // denominators could not provide one exact shared Sashimi scale or a stable page
            // content identity, so reject it at publication rather than reconciling downstream.
            return Err(ExactSummaryRootError::ContractMismatch);
        }
        let expanded_junction_rows = expanded_junction_row_assignments(&junctions)?;
        for tile in &pairs {
            let expected = ExactPairSummaryKey::new(
                source_revision,
                record_contract,
                contract.pairs(),
                tile.tile(),
            );
            if tile.key() != expected {
                return Err(ExactSummaryRootError::ContractMismatch);
            }
        }
        let filter_coverage_fingerprint = filters
            .first()
            .map(|tile| tile.coverage_fingerprint())
            .ok_or(ExactSummaryRootError::IncompleteSummaryFamily)?;
        if filters
            .iter()
            .any(|tile| tile.coverage_fingerprint() != filter_coverage_fingerprint)
        {
            return Err(ExactSummaryRootError::ContractMismatch);
        }
        for tile in &filters {
            let expected = ExactFilterSummaryKey::new(
                source_revision,
                filter_coverage_fingerprint,
                record_contract,
                contract.filters(),
                prepared,
                tile.tile(),
            );
            if tile.key() != expected {
                return Err(ExactSummaryRootError::ContractMismatch);
            }
        }
        validate_filter_provenance_ownership(&filters, prepared, source_revision, record_contract)?;
        if let Some(modification_contract) = contract.modifications() {
            for tile in &modifications {
                let expected = ExactModificationSummaryKey::new(
                    source_revision,
                    record_contract,
                    modification_contract,
                    tile.tile(),
                );
                if tile.key() != expected {
                    return Err(ExactSummaryRootError::ContractMismatch);
                }
            }
        }

        let prepared_pair_summary = summarize_pair_candidate_iter(
            pairs
                .iter()
                .flat_map(|tile| tile.candidates().iter().copied())
                .filter(|candidate| {
                    candidate.record_start < prepared.end()
                        && candidate.record_end > prepared.start()
                }),
            true,
        );
        let overview_depths = project_overview_depths(&depth, prepared);

        let metadata_bytes = root_metadata_bytes(
            depth.len(),
            alleles.len(),
            junctions.len(),
            pairs.len(),
            filters.len(),
            modifications.len(),
            expanded_junction_rows.len(),
            overview_depths.len(),
        );
        let mut reservation = ledger.reserve(
            AllocationClass::SummaryRootMetadata,
            metadata_bytes,
            reservation_kind,
        )?;
        let metadata_allocation = reservation.register(metadata_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            revision,
            evidence_revision,
            source_revision,
            record_contract,
            contract,
            prepared,
            depth: depth.into(),
            alleles: alleles.into(),
            junctions: junctions.into(),
            expanded_junction_rows: expanded_junction_rows.into(),
            pairs: pairs.into(),
            filters: filters.into(),
            modifications: modifications.into(),
            prepared_pair_summary,
            overview_depths: overview_depths.into(),
            metadata_allocation,
        }))
    }

    #[must_use]
    pub const fn revision(&self) -> SummaryRevision {
        self.revision
    }

    #[must_use]
    pub const fn evidence_revision(&self) -> EvidenceRevision {
        self.evidence_revision
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn record_contract(&self) -> ContractFingerprint {
        self.record_contract
    }

    #[must_use]
    pub const fn contract(&self) -> SummaryContract {
        self.contract
    }

    #[must_use]
    pub const fn prepared_interval(&self) -> GenomicInterval {
        self.prepared
    }

    pub(crate) const fn metadata_allocation(&self) -> &AllocationLease {
        &self.metadata_allocation
    }

    #[must_use]
    pub fn depth_tiles(&self) -> &[Arc<ExactDepthSummaryTile>] {
        &self.depth
    }

    /// Fixed-size exact depth overview for the whole prepared root. Its bin order spans
    /// [`Self::prepared_interval`] and its construction is never charged to a UI frame.
    #[must_use]
    pub fn overview_depths(&self) -> &[u32] {
        &self.overview_depths
    }

    #[must_use]
    pub fn allele_tiles(&self) -> &[Arc<ExactAlleleSummaryTile>] {
        &self.alleles
    }

    #[must_use]
    pub fn junction_tiles(&self) -> &[Arc<ExactJunctionSummaryTile>] {
        &self.junctions
    }

    /// Stable expanded row for a junction owned by this immutable root.
    #[must_use]
    pub fn expanded_junction_row(&self, start: u64, end: u64) -> Option<u32> {
        self.expanded_junction_rows
            .binary_search_by_key(&(start, end), |assignment| {
                (assignment.start, assignment.end)
            })
            .ok()
            .map(|index| self.expanded_junction_rows[index].row)
    }

    #[must_use]
    pub fn expanded_junction_row_count(&self) -> u32 {
        self.expanded_junction_rows
            .iter()
            .map(|assignment| assignment.row)
            .max()
            .map_or(1, |row| row.saturating_add(1))
    }

    /// Exact tile provenance for the canonical owner of a junction start. This is suitable for
    /// scene-backed inspection; callers must not reinterpret it as a fresh viewport query.
    #[must_use]
    pub fn junction_provenance_at(&self, start: u64) -> Option<JunctionProvenance> {
        self.junctions
            .iter()
            .find(|tile| {
                let tile = tile.tile();
                tile.coordinate()
                    .checked_mul(tile.bases().get())
                    .is_some_and(|tile_start| {
                        tile_start <= start && start < tile_start.saturating_add(tile.bases().get())
                    })
            })
            .map(|tile| tile.summary().provenance)
    }

    /// Whole-file mapped-record normalization authority shared by every exact junction tile.
    /// Mixed or missing metadata is reported as unavailable rather than selecting one tile's
    /// value opportunistically.
    #[must_use]
    pub fn junction_normalization_mapped_records(&self) -> Option<u64> {
        let mut normalization = None;
        for tile in self.junctions.iter() {
            let candidate = tile.summary().provenance.normalization_mapped_records?;
            if normalization.is_some_and(|current| current != candidate) {
                return None;
            }
            normalization = Some(candidate);
        }
        normalization
    }

    #[must_use]
    pub fn pair_tiles(&self) -> &[Arc<ExactPairSummaryTile>] {
        &self.pairs
    }

    /// Exact pair statistics for this root's complete immutable prepared horizon. This derivative
    /// is built once beside the root and is therefore safe to consume from a native UI frame.
    #[must_use]
    pub const fn prepared_pair_summary(&self) -> PairSummary {
        self.prepared_pair_summary
    }

    #[must_use]
    pub fn filter_tiles(&self) -> &[Arc<ExactFilterSummaryTile>] {
        &self.filters
    }

    #[must_use]
    pub fn modification_tiles(&self) -> &[Arc<ExactModificationSummaryTile>] {
        &self.modifications
    }

    /// Exact modification summary and canonical owner interval containing `position`.
    #[must_use]
    pub fn modification_summary_at(
        &self,
        position: u64,
    ) -> Option<(GenomicInterval, &ModificationSummary)> {
        if !self.prepared.contains(position) {
            return None;
        }
        self.modifications.iter().find_map(|tile| {
            let canonical = tile.tile();
            let start = canonical
                .coordinate()
                .checked_mul(canonical.bases().get())?;
            let end = start
                .saturating_add(canonical.bases().get())
                .min(self.prepared.end());
            (start <= position && position < end)
                .then(|| GenomicInterval::new(self.prepared.contig(), start, end).ok())
                .flatten()
                .map(|interval| (interval, tile.summary()))
        })
    }

    /// Exact per-tile modified-base summaries overlapping `interval`, paired with the genomic
    /// interval used to build each summary. Keeping this coordinate derivation inside the root
    /// prevents exporters from independently reconstructing canonical tile ownership.
    pub fn modification_summaries(
        &self,
        interval: GenomicInterval,
    ) -> Result<Vec<(GenomicInterval, &ModificationSummary)>, ExactSummaryQueryError> {
        self.validate_query(interval)?;
        let mut summaries = Vec::new();
        for tile in self.modifications.iter() {
            let start = tile
                .tile()
                .coordinate()
                .checked_mul(tile.tile().bases().get())
                .ok_or(ExactSummaryQueryError::CoordinateOverflow)?;
            let length = u64::try_from(tile.summary().bin_count())
                .map_err(|_| ExactSummaryQueryError::CoordinateOverflow)?;
            let end = start
                .checked_add(length)
                .ok_or(ExactSummaryQueryError::CoordinateOverflow)?;
            let owner = GenomicInterval::new(self.prepared.contig(), start, end)
                .map_err(|_| ExactSummaryQueryError::CoordinateOverflow)?;
            if owner.overlaps(interval) {
                summaries.push((owner, tile.summary()));
            }
        }
        summaries.sort_unstable_by_key(|(owner, _)| (owner.start(), owner.end()));
        Ok(summaries)
    }

    pub fn maximum_depth(&self, interval: GenomicInterval) -> Result<u32, ExactSummaryQueryError> {
        self.validate_query(interval)?;
        Ok(self
            .depth
            .iter()
            .filter_map(|tile| tile.depth().maximum_depth(interval))
            .max()
            .unwrap_or_default())
    }

    pub fn project_depth_bins(
        &self,
        interval: GenomicInterval,
        bins: usize,
    ) -> Result<Vec<PileupBin>, ExactSummaryQueryError> {
        self.validate_query(interval)?;
        let mut output = vec![PileupBin::default(); bins];
        for tile in self
            .depth
            .iter()
            .filter(|tile| tile.tile().overlaps(interval))
        {
            let projected = tile.depth().project_bins(interval, bins);
            for (output, candidate) in output.iter_mut().zip(projected) {
                if candidate.depth > output.depth {
                    output.depth = candidate.depth;
                    output.forward_depth = candidate.forward_depth;
                    output.reverse_depth = candidate.reverse_depth;
                    output.mapping_quality_sum = candidate.mapping_quality_sum;
                    output.mapping_quality_observations = candidate.mapping_quality_observations;
                }
            }
        }
        Ok(output)
    }

    pub fn allele_site(&self, position: u64) -> Result<Option<PileupBin>, ExactSummaryQueryError> {
        if !self.prepared.contains(position) {
            return Err(ExactSummaryQueryError::OutsidePreparedInterval);
        }
        let position_interval = single_base(self.prepared, position)?;
        Ok(self
            .alleles
            .iter()
            .find(|tile| tile.tile().overlaps(position_interval))
            .and_then(|tile| tile.site_at(position)))
    }

    /// Returns the exact canonical-tile evidence for `position` without constructing a mutable
    /// viewport query. A missing result inside the prepared interval means allele detail was not
    /// requested for this scene or the position has no allele tile.
    #[must_use]
    pub fn allele_site_evidence_at(&self, position: u64) -> Option<ExactAlleleSiteEvidence> {
        if !self.prepared.contains(position) {
            return None;
        }
        self.alleles.iter().find_map(|tile| {
            let canonical = tile.tile();
            let start = canonical
                .coordinate()
                .checked_mul(canonical.bases().get())?;
            let end = start.saturating_add(tile.sites().len() as u64);
            if !(start <= position && position < end) {
                return None;
            }
            Some(ExactAlleleSiteEvidence {
                owner_interval: GenomicInterval::new(self.prepared.contig(), start, end).ok()?,
                bin: tile.site_at(position)?,
                eligible_records: tile.eligible_records(),
                collapsed_mate_bases: tile.collapsed_mate_bases(),
            })
        })
    }

    /// Returns exact aligned-base depth at `position` and merges exact allele observations when
    /// that independently keyed family is present. No viewport query or display binning authority
    /// participates in this lookup.
    #[must_use]
    pub fn pileup_site_evidence_at(&self, position: u64) -> Option<ExactPileupSiteEvidence> {
        if !self.prepared.contains(position) {
            return None;
        }
        let end = position.checked_add(1)?;
        let interval = GenomicInterval::new(self.prepared.contig(), position, end).ok()?;
        let depth = self
            .depth
            .iter()
            .find(|tile| tile.depth().interval().contains(position))?;
        let depth_bin = depth.depth().project_bins(interval, 1).into_iter().next()?;
        let allele = self.allele_site_evidence_at(position);
        let mut bin = allele.map_or_else(PileupBin::default, |evidence| evidence.bin);
        bin.depth = depth_bin.depth;
        bin.forward_depth = depth_bin.forward_depth;
        bin.reverse_depth = depth_bin.reverse_depth;
        bin.mapping_quality_sum = depth_bin.mapping_quality_sum;
        bin.mapping_quality_observations = depth_bin.mapping_quality_observations;
        Some(ExactPileupSiteEvidence {
            owner_interval: depth.depth().interval(),
            bin,
            eligible_records: depth.eligible_records(),
            collapsed_mate_bases: depth.collapsed_mate_bases(),
            allele_observations_available: allele.is_some(),
        })
    }

    pub fn junctions(
        &self,
        interval: GenomicInterval,
    ) -> Result<Vec<JunctionEvidence>, ExactSummaryQueryError> {
        let mut junctions = Vec::with_capacity(self.junction_count(interval)?);
        self.for_each_junction(interval, |junction| junctions.push(junction))?;
        junctions.sort_unstable_by_key(|junction| (junction.start, junction.end));
        Ok(junctions)
    }

    pub fn junction_count(
        &self,
        interval: GenomicInterval,
    ) -> Result<usize, ExactSummaryQueryError> {
        self.validate_query(interval)?;
        Ok(self
            .junctions
            .iter()
            .flat_map(|tile| tile.summary().junctions.iter())
            .filter(|junction| junction.start < interval.end() && junction.end > interval.start())
            .count())
    }

    pub fn for_each_junction(
        &self,
        interval: GenomicInterval,
        mut visit: impl FnMut(JunctionEvidence),
    ) -> Result<(), ExactSummaryQueryError> {
        self.validate_query(interval)?;
        for junction in self
            .junctions
            .iter()
            .flat_map(|tile| tile.summary().junctions.iter().copied())
            .filter(|junction| junction.start < interval.end() && junction.end > interval.start())
        {
            visit(junction);
        }
        Ok(())
    }

    pub fn pair_summary(
        &self,
        interval: GenomicInterval,
    ) -> Result<PairSummary, ExactSummaryQueryError> {
        self.validate_query(interval)?;
        let candidates = self
            .pairs
            .iter()
            .flat_map(|tile| tile.candidates().iter().copied())
            .filter(|candidate| {
                candidate.record_start < interval.end() && candidate.record_end > interval.start()
            });
        Ok(summarize_pair_candidate_iter(candidates, true))
    }

    /// Returns exact provenance only for a union of complete canonical filter tiles.
    pub fn filter_provenance(
        &self,
        interval: GenomicInterval,
    ) -> Result<FilteredRecordProvenance, ExactSummaryQueryError> {
        self.validate_query(interval)?;
        if interval != self.prepared {
            return Err(ExactSummaryQueryError::IndivisibleFilterProvenance);
        }
        let mut cursor = interval.start();
        let mut provenance = FilteredRecordProvenance::default();
        while cursor < interval.end() {
            let certificate = self
                .filters
                .iter()
                .flat_map(|tile| tile.provenances())
                .filter(|certificate| certificate.interval().contig() == interval.contig())
                .filter(|certificate| certificate.interval().start() == cursor)
                .filter(|certificate| certificate.interval().end() <= interval.end())
                .max_by_key(|certificate| certificate.interval().end())
                .ok_or(ExactSummaryQueryError::IndivisibleFilterProvenance)?;
            provenance = provenance.saturating_add(certificate.provenance());
            cursor = certificate.interval().end();
        }
        Ok(provenance)
    }

    /// Exact transaction certificates retained by the scene. Overlapping certificates remain
    /// separate facts; consumers must not add them unless `filter_provenance` proves a partition.
    pub fn filter_provenance_certificates(
        &self,
    ) -> impl Iterator<Item = crate::CompleteCoverage> + '_ {
        self.filters
            .iter()
            .flat_map(|tile| tile.provenances().iter().copied())
    }

    pub fn pin(&self) -> Result<ExactSummaryPins, LedgerError> {
        let count = 1_usize
            .saturating_add(self.depth.len())
            .saturating_add(self.alleles.len())
            .saturating_add(self.junctions.len())
            .saturating_add(self.pairs.len())
            .saturating_add(self.filters.len())
            .saturating_add(self.modifications.len());
        let mut allocations = Vec::with_capacity(count);
        allocations.push(self.metadata_allocation.pin()?);
        for tile in self.depth.iter() {
            allocations.push(tile.pin()?);
        }
        for tile in self.alleles.iter() {
            allocations.push(tile.pin()?);
        }
        for tile in self.junctions.iter() {
            allocations.push(tile.pin()?);
        }
        for tile in self.pairs.iter() {
            allocations.push(tile.pin()?);
        }
        for tile in self.filters.iter() {
            allocations.push(tile.pin()?);
        }
        for tile in self.modifications.iter() {
            allocations.push(tile.pin()?);
        }
        Ok(ExactSummaryPins { allocations })
    }

    #[must_use]
    pub fn estimated_metadata_bytes(&self) -> usize {
        root_metadata_bytes(
            self.depth.len(),
            self.alleles.len(),
            self.junctions.len(),
            self.pairs.len(),
            self.filters.len(),
            self.modifications.len(),
            self.expanded_junction_rows.len(),
            self.overview_depths.len(),
        )
    }

    fn validate_query(&self, interval: GenomicInterval) -> Result<(), ExactSummaryQueryError> {
        if !self.prepared.contains_interval(interval) {
            return Err(ExactSummaryQueryError::OutsidePreparedInterval);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct ExactSummaryPins {
    allocations: Vec<PinnedAllocationLease>,
}

impl ExactSummaryPins {
    #[must_use]
    pub fn len(&self) -> usize {
        self.allocations.len()
    }

    pub(crate) fn allocations(&self) -> &[PinnedAllocationLease] {
        &self.allocations
    }
}

fn validate_contiguous_depth(
    tiles: &[Arc<ExactDepthSummaryTile>],
) -> Result<GenomicInterval, ExactSummaryRootError> {
    let first = tiles
        .first()
        .ok_or(ExactSummaryRootError::MissingDepthTiles)?
        .depth()
        .interval();
    let mut end = first.end();
    for tile in tiles.iter().skip(1) {
        let interval = tile.depth().interval();
        if interval.contig() != first.contig() || interval.start() != end {
            return Err(ExactSummaryRootError::NonContiguousDepthTiles);
        }
        end = interval.end();
    }
    GenomicInterval::new(first.contig(), first.start(), end)
        .map_err(|_| ExactSummaryRootError::NonContiguousDepthTiles)
}

fn validate_family_tiles<T>(
    expected: &[crate::CanonicalGenomicTile],
    actual: &[Arc<T>],
    required: bool,
    tile: impl Fn(&T) -> crate::CanonicalGenomicTile,
) -> Result<(), ExactSummaryRootError> {
    if !required {
        return actual
            .is_empty()
            .then_some(())
            .ok_or(ExactSummaryRootError::UnexpectedSummaryFamily);
    }
    if actual.len() != expected.len()
        || actual
            .iter()
            .zip(expected)
            .any(|(actual, expected)| tile(actual) != *expected)
    {
        return Err(ExactSummaryRootError::IncompleteSummaryFamily);
    }
    Ok(())
}

fn validate_filter_provenance_ownership(
    filters: &[Arc<ExactFilterSummaryTile>],
    prepared: GenomicInterval,
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
) -> Result<(), ExactSummaryRootError> {
    let mut certificates = Vec::<crate::CompleteCoverage>::new();
    for tile in filters {
        if tile.provenance_scope() != prepared {
            return Err(ExactSummaryRootError::InvalidFilterProvenanceOwnership);
        }
        let tile_interval = tile
            .tile()
            .interval(prepared.end())
            .map_err(|_| ExactSummaryRootError::InvalidFilterProvenanceOwnership)?;
        for certificate in tile.provenances().iter().copied() {
            let owner = certificate.interval().start().max(prepared.start());
            if certificate.source_revision() != source_revision
                || certificate.record_contract() != record_contract
                || !certificate.interval().overlaps(prepared)
                || !tile_interval.contains(owner)
                || certificates.contains(&certificate)
            {
                return Err(ExactSummaryRootError::InvalidFilterProvenanceOwnership);
            }
            certificates.push(certificate);
        }
    }
    if certificates.is_empty() {
        return Err(ExactSummaryRootError::InvalidFilterProvenanceOwnership);
    }
    let mut cursor = prepared.start();
    while cursor < prepared.end() {
        let next = certificates
            .iter()
            .filter(|certificate| certificate.interval().start() <= cursor)
            .filter(|certificate| certificate.interval().end() > cursor)
            .map(|certificate| certificate.interval().end())
            .max()
            .unwrap_or(cursor);
        if next == cursor {
            return Err(ExactSummaryRootError::InvalidFilterProvenanceOwnership);
        }
        cursor = next;
    }
    Ok(())
}

fn root_metadata_bytes(
    depth: usize,
    alleles: usize,
    junctions: usize,
    pairs: usize,
    filters: usize,
    modifications: usize,
    expanded_junction_rows: usize,
    overview_depths: usize,
) -> usize {
    size_of::<ExactSummaryRoot>()
        .saturating_add(
            depth
                .saturating_add(alleles)
                .saturating_add(junctions)
                .saturating_add(pairs)
                .saturating_add(filters)
                .saturating_add(modifications)
                .saturating_mul(size_of::<Arc<ExactDepthSummaryTile>>()),
        )
        .saturating_add(
            expanded_junction_rows.saturating_mul(size_of::<ExpandedJunctionRowAssignment>()),
        )
        .saturating_add(overview_depths.saturating_mul(size_of::<u32>()))
}

fn project_overview_depths(
    depth: &[Arc<ExactDepthSummaryTile>],
    prepared: GenomicInterval,
) -> Vec<u32> {
    let bins = usize::try_from(prepared.len().min(EXACT_SUMMARY_OVERVIEW_BINS as u64))
        .unwrap_or(EXACT_SUMMARY_OVERVIEW_BINS);
    let mut output = vec![0_u32; bins];
    for tile in depth {
        for (output, candidate) in output
            .iter_mut()
            .zip(tile.depth().project_bins(prepared, bins))
        {
            *output = (*output).max(candidate.depth);
        }
    }
    output
}

fn expanded_junction_row_assignments(
    junctions: &[Arc<ExactJunctionSummaryTile>],
) -> Result<Vec<ExpandedJunctionRowAssignment>, ExactSummaryRootError> {
    let mut spans = junctions
        .iter()
        .flat_map(|tile| tile.summary().junctions.iter())
        .map(|junction| (junction.start, junction.end))
        .collect::<Vec<_>>();
    spans.sort_unstable();
    spans.dedup();
    let mut row_ends = Vec::<u64>::new();
    let mut assignments = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        let row = if let Some((row, row_end)) = row_ends
            .iter_mut()
            .enumerate()
            .find(|(_, row_end)| **row_end <= start)
        {
            *row_end = end;
            u32::try_from(row).map_err(|_| ExactSummaryRootError::JunctionRowOverflow)?
        } else {
            row_ends.push(end);
            u32::try_from(row_ends.len().saturating_sub(1))
                .map_err(|_| ExactSummaryRootError::JunctionRowOverflow)?
        };
        assignments.push(ExpandedJunctionRowAssignment { start, end, row });
    }
    Ok(assignments)
}

fn single_base(
    prepared: GenomicInterval,
    position: u64,
) -> Result<GenomicInterval, ExactSummaryQueryError> {
    GenomicInterval::new(
        prepared.contig(),
        position,
        position
            .checked_add(1)
            .ok_or(ExactSummaryQueryError::OutsidePreparedInterval)?,
    )
    .map_err(|_| ExactSummaryQueryError::OutsidePreparedInterval)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ExactSummaryRootError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("exact summary root requires at least one depth tile")]
    MissingDepthTiles,
    #[error("exact summary depth tiles are not one contiguous canonical horizon")]
    NonContiguousDepthTiles,
    #[error("summary target is outside the prepared canonical horizon")]
    TargetOutsidePreparedInterval,
    #[error("a required exact summary family does not cover every canonical tile")]
    IncompleteSummaryFamily,
    #[error("a disabled exact summary family was unexpectedly supplied")]
    UnexpectedSummaryFamily,
    #[error("exact summary tile source or typed contract does not match its root")]
    ContractMismatch,
    #[error("filter provenance must have one owner for the complete prepared horizon")]
    InvalidFilterProvenanceOwnership,
    #[error("expanded junction row identity exceeded the supported address space")]
    JunctionRowOverflow,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ExactSummaryQueryError {
    #[error("summary query is outside the prepared canonical horizon")]
    OutsidePreparedInterval,
    #[error("filter provenance is only exact for its complete certified horizon")]
    IndivisibleFilterProvenance,
    #[error("summary coordinates exceed the supported genomic address space")]
    CoordinateOverflow,
}
