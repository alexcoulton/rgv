use std::{mem::size_of, sync::Arc};

use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    CanonicalGenomicTile, CanonicalTileError, CompleteCoverage, ContractFingerprint,
    EvidenceRevision, EvidenceRoot, FilterSummaryContract, FilteredRecordProvenance, LedgerError,
    PinnedAllocationLease, ReservationKind, TransientResourceClass, fingerprint::StableHasher,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExactFilterSummaryKey {
    source_revision: AlignmentSourceRevision,
    coverage_fingerprint: ContractFingerprint,
    record_contract: ContractFingerprint,
    filter_contract: ContractFingerprint,
    provenance_scope: rgv_core::GenomicInterval,
    tile: CanonicalGenomicTile,
}

impl ExactFilterSummaryKey {
    #[must_use]
    pub fn new(
        source_revision: AlignmentSourceRevision,
        coverage_fingerprint: ContractFingerprint,
        record_contract: ContractFingerprint,
        contract: FilterSummaryContract,
        provenance_scope: rgv_core::GenomicInterval,
        tile: CanonicalGenomicTile,
    ) -> Self {
        Self {
            source_revision,
            coverage_fingerprint,
            record_contract,
            filter_contract: contract.fingerprint(),
            provenance_scope,
            tile,
        }
    }
}

/// Exact included/excluded record provenance for one fully certified canonical tile.
#[derive(Debug)]
pub struct ExactFilterSummaryTile {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    coverage_fingerprint: ContractFingerprint,
    record_contract: ContractFingerprint,
    contract: FilterSummaryContract,
    provenance_scope: rgv_core::GenomicInterval,
    tile: CanonicalGenomicTile,
    /// Exact, indivisible source transactions. Each certificate is owned by exactly one summary
    /// tile using its clamped interval start; counts are never spatially apportioned or silently
    /// summed across overlapping queries.
    provenances: Arc<[CompleteCoverage]>,
    allocation: AllocationLease,
}

impl ExactFilterSummaryTile {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        provenance_scope: rgv_core::GenomicInterval,
        contract: FilterSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactFilterSummaryError> {
        let interval = tile.interval(contig_length)?;
        if !provenance_scope.contains_interval(interval) {
            return Err(ExactFilterSummaryError::TileOutsideProvenanceScope);
        }
        let coverage_fingerprint = filter_provenance_fingerprint(evidence, provenance_scope);
        let maximum = evidence
            .coverage()
            .certificates()
            .iter()
            .filter(|certificate| certificate.interval().overlaps(provenance_scope))
            .filter(|certificate| {
                interval.contains(certificate.interval().start().max(provenance_scope.start()))
            })
            .count();
        let maximum_bytes =
            size_of::<Self>().saturating_add(maximum.saturating_mul(size_of::<CompleteCoverage>()));
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::ExactSummaryBuilder,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut reservation = ledger.reserve(
            AllocationClass::ExactSummary,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut provenances = evidence
            .coverage()
            .certificates()
            .iter()
            .copied()
            .filter(|certificate| certificate.interval().overlaps(provenance_scope))
            .filter(|certificate| {
                interval.contains(certificate.interval().start().max(provenance_scope.start()))
            })
            .collect::<Vec<_>>();
        provenances.sort_unstable_by_key(|certificate| {
            (certificate.interval().start(), certificate.interval().end())
        });
        provenances.shrink_to_fit();
        let retained_bytes = size_of::<Self>().saturating_add(
            provenances
                .capacity()
                .saturating_mul(size_of::<CompleteCoverage>()),
        );
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            source_revision: evidence.source_revision(),
            evidence_revision: evidence.revision(),
            coverage_fingerprint,
            record_contract: evidence.record_contract(),
            contract,
            provenance_scope,
            tile,
            provenances: provenances.into(),
            allocation,
        }))
    }

    #[must_use]
    pub fn key(&self) -> ExactFilterSummaryKey {
        ExactFilterSummaryKey::new(
            self.source_revision,
            self.coverage_fingerprint,
            self.record_contract,
            self.contract,
            self.provenance_scope,
            self.tile,
        )
    }

    #[must_use]
    pub const fn evidence_revision(&self) -> EvidenceRevision {
        self.evidence_revision
    }

    #[must_use]
    pub const fn tile(&self) -> CanonicalGenomicTile {
        self.tile
    }

    #[must_use]
    pub const fn provenance_scope(&self) -> rgv_core::GenomicInterval {
        self.provenance_scope
    }

    #[must_use]
    pub const fn coverage_fingerprint(&self) -> ContractFingerprint {
        self.coverage_fingerprint
    }

    #[must_use]
    pub fn provenances(&self) -> &[CompleteCoverage] {
        &self.provenances
    }

    /// Compatibility accessor for a genuinely single-certificate scope. Composite scopes must
    /// use `provenances` or the root's exact partition check.
    #[must_use]
    pub fn provenance(&self) -> Option<FilteredRecordProvenance> {
        (self.provenances.len() == 1).then(|| self.provenances[0].provenance())
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }

    pub fn pin(&self) -> Result<PinnedAllocationLease, LedgerError> {
        self.allocation.pin()
    }

    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(
            self.provenances
                .len()
                .saturating_mul(size_of::<CompleteCoverage>()),
        )
    }
}

pub(crate) fn filter_provenance_fingerprint(
    evidence: &EvidenceRoot,
    scope: rgv_core::GenomicInterval,
) -> ContractFingerprint {
    let mut hash = StableHasher::default();
    hash.write(b"exact-filter-provenance-certificates-v1\0");
    for certificate in evidence
        .coverage()
        .certificates()
        .iter()
        .filter(|certificate| certificate.interval().overlaps(scope))
    {
        let interval = certificate.interval();
        let provenance = certificate.provenance();
        hash.write(&interval.contig().get().to_le_bytes());
        hash.write(&interval.start().to_le_bytes());
        hash.write(&interval.end().to_le_bytes());
        for value in [
            provenance.records_seen,
            provenance.eligible_records,
            provenance.unmapped,
            provenance.secondary,
            provenance.supplementary,
            provenance.user_filter,
        ] {
            hash.write(&value.to_le_bytes());
        }
    }
    hash.finish()
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ExactFilterSummaryError {
    #[error(transparent)]
    Tile(#[from] CanonicalTileError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("coverage certificates cannot be exactly partitioned into this canonical tile")]
    IndivisibleCoverageCertificate,
    #[error("canonical filter tile lies outside its indivisible provenance scope")]
    TileOutsideProvenanceScope,
}
