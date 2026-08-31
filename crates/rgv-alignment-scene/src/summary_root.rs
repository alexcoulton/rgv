use std::{mem::size_of, sync::Arc};

use rgv_core::GenomicInterval;
use rgv_hts::{JunctionEvidence, PairSummary, PileupBin};
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
    pairs: Arc<[Arc<ExactPairSummaryTile>]>,
    filters: Arc<[Arc<ExactFilterSummaryTile>]>,
    modifications: Arc<[Arc<ExactModificationSummaryTile>]>,
    metadata_allocation: AllocationLease,
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

        let metadata_bytes = root_metadata_bytes(
            depth.len(),
            alleles.len(),
            junctions.len(),
            pairs.len(),
            filters.len(),
            modifications.len(),
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
            pairs: pairs.into(),
            filters: filters.into(),
            modifications: modifications.into(),
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

    #[must_use]
    pub fn depth_tiles(&self) -> &[Arc<ExactDepthSummaryTile>] {
        &self.depth
    }

    #[must_use]
    pub fn allele_tiles(&self) -> &[Arc<ExactAlleleSummaryTile>] {
        &self.alleles
    }

    #[must_use]
    pub fn junction_tiles(&self) -> &[Arc<ExactJunctionSummaryTile>] {
        &self.junctions
    }

    #[must_use]
    pub fn pair_tiles(&self) -> &[Arc<ExactPairSummaryTile>] {
        &self.pairs
    }

    #[must_use]
    pub fn filter_tiles(&self) -> &[Arc<ExactFilterSummaryTile>] {
        &self.filters
    }

    #[must_use]
    pub fn modification_tiles(&self) -> &[Arc<ExactModificationSummaryTile>] {
        &self.modifications
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
) -> usize {
    size_of::<ExactSummaryRoot>().saturating_add(
        depth
            .saturating_add(alleles)
            .saturating_add(junctions)
            .saturating_add(pairs)
            .saturating_add(filters)
            .saturating_add(modifications)
            .saturating_mul(size_of::<Arc<ExactDepthSummaryTile>>()),
    )
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
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ExactSummaryQueryError {
    #[error("summary query is outside the prepared canonical horizon")]
    OutsidePreparedInterval,
    #[error("filter provenance is only exact for its complete certified horizon")]
    IndivisibleFilterProvenance,
}
