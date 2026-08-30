use rgv_core::{ContigId, GenomicInterval};
use thiserror::Error;

use crate::{AlignmentSourceRevision, ContractFingerprint};

/// Exact provenance for records excluded by the active record contract.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FilteredRecordProvenance {
    pub records_seen: u64,
    pub eligible_records: u64,
    pub unmapped: u64,
    pub secondary: u64,
    pub supplementary: u64,
    pub user_filter: u64,
}

impl FilteredRecordProvenance {
    #[must_use]
    pub const fn accounted_records(self) -> u64 {
        self.eligible_records
            .saturating_add(self.unmapped)
            .saturating_add(self.secondary)
            .saturating_add(self.supplementary)
            .saturating_add(self.user_filter)
    }

    #[must_use]
    pub const fn is_exact(self) -> bool {
        self.records_seen == self.accounted_records()
    }
}

/// Proof that a source actor completely evaluated one interval under one record contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompleteCoverage {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    interval: GenomicInterval,
    provenance: FilteredRecordProvenance,
}

impl CompleteCoverage {
    pub fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        interval: GenomicInterval,
        provenance: FilteredRecordProvenance,
    ) -> Result<Self, CoverageError> {
        if !provenance.is_exact() {
            return Err(CoverageError::IncompleteProvenance {
                seen: provenance.records_seen,
                accounted: provenance.accounted_records(),
            });
        }
        Ok(Self {
            source_revision,
            record_contract,
            interval,
            provenance,
        })
    }

    #[must_use]
    pub const fn source_revision(self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn record_contract(self) -> ContractFingerprint {
        self.record_contract
    }

    #[must_use]
    pub const fn interval(self) -> GenomicInterval {
        self.interval
    }

    #[must_use]
    pub const fn provenance(self) -> FilteredRecordProvenance {
        self.provenance
    }
}

/// Canonical union of certified intervals for one source revision and record contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedCoverageSet {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    intervals: Vec<GenomicInterval>,
    certificates: Vec<CompleteCoverage>,
}

impl CertifiedCoverageSet {
    #[must_use]
    pub const fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
    ) -> Self {
        Self {
            source_revision,
            record_contract,
            intervals: Vec::new(),
            certificates: Vec::new(),
        }
    }

    pub fn insert(&mut self, coverage: CompleteCoverage) -> Result<(), CoverageError> {
        if coverage.source_revision != self.source_revision {
            return Err(CoverageError::SourceRevisionMismatch);
        }
        if coverage.record_contract != self.record_contract {
            return Err(CoverageError::RecordContractMismatch);
        }
        if let Some(existing) = self
            .certificates
            .iter()
            .find(|existing| existing.interval == coverage.interval)
        {
            if *existing == coverage {
                return Ok(());
            }
            return Err(CoverageError::ConflictingCertificate(coverage.interval));
        }
        self.certificates.push(coverage);
        self.certificates.sort_by_key(|certificate| {
            (
                certificate.interval.contig().get(),
                certificate.interval.start(),
                certificate.interval.end(),
            )
        });
        self.intervals.push(coverage.interval);
        self.intervals
            .sort_by_key(|interval| (interval.contig().get(), interval.start(), interval.end()));
        let mut merged = Vec::<GenomicInterval>::with_capacity(self.intervals.len());
        for interval in self.intervals.drain(..) {
            let Some(previous) = merged.last_mut() else {
                merged.push(interval);
                continue;
            };
            if previous.contig() == interval.contig() && interval.start() <= previous.end() {
                *previous = GenomicInterval::new(
                    previous.contig(),
                    previous.start(),
                    previous.end().max(interval.end()),
                )
                .expect("merged complete intervals remain non-empty");
            } else {
                merged.push(interval);
            }
        }
        self.intervals = merged;
        Ok(())
    }

    #[must_use]
    pub fn covers(&self, interval: GenomicInterval) -> bool {
        self.intervals
            .iter()
            .any(|certified| certified.contains_interval(interval))
    }

    #[must_use]
    pub fn intervals(&self) -> &[GenomicInterval] {
        &self.intervals
    }

    #[must_use]
    pub fn certificates(&self) -> &[CompleteCoverage] {
        &self.certificates
    }

    /// Returns exact filter provenance when non-overlapping retained certificates partition the
    /// requested interval. A larger indivisible certificate is not silently apportioned.
    #[must_use]
    pub fn exact_provenance(&self, interval: GenomicInterval) -> Option<FilteredRecordProvenance> {
        let mut cursor = interval.start();
        let mut provenance = FilteredRecordProvenance::default();
        while cursor < interval.end() {
            let next = self
                .certificates
                .iter()
                .filter(|certificate| certificate.interval.contig() == interval.contig())
                .filter(|certificate| certificate.interval.start() == cursor)
                .filter(|certificate| certificate.interval.end() <= interval.end())
                .max_by_key(|certificate| certificate.interval.end())?;
            provenance = provenance.saturating_add(next.provenance);
            cursor = next.interval.end();
        }
        Some(provenance)
    }

    #[must_use]
    pub fn covered_bases(&self, contig: ContigId) -> u64 {
        self.intervals
            .iter()
            .filter(|interval| interval.contig() == contig)
            .fold(0_u64, |total, interval| {
                total.saturating_add(interval.len())
            })
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CoverageError {
    #[error("filtered-record provenance is incomplete: saw {seen}, accounted for {accounted}")]
    IncompleteProvenance { seen: u64, accounted: u64 },
    #[error("coverage belongs to a different source revision")]
    SourceRevisionMismatch,
    #[error("coverage belongs to a different record contract")]
    RecordContractMismatch,
    #[error("coverage interval {0:?} has conflicting exact provenance")]
    ConflictingCertificate(GenomicInterval),
}

impl FilteredRecordProvenance {
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self {
            records_seen: self.records_seen.saturating_add(other.records_seen),
            eligible_records: self.eligible_records.saturating_add(other.eligible_records),
            unmapped: self.unmapped.saturating_add(other.unmapped),
            secondary: self.secondary.saturating_add(other.secondary),
            supplementary: self.supplementary.saturating_add(other.supplementary),
            user_filter: self.user_filter.saturating_add(other.user_filter),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RevisionFingerprint, SourceFingerprint};

    fn revision(value: u64) -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, value),
        )
    }

    fn certificate(start: u64, end: u64) -> CompleteCoverage {
        CompleteCoverage::new(
            revision(4),
            ContractFingerprint::new(5),
            GenomicInterval::new(ContigId::new(0), start, end).unwrap(),
            FilteredRecordProvenance {
                records_seen: 10,
                eligible_records: 7,
                user_filter: 3,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn certificate_requires_exact_provenance() {
        let error = CompleteCoverage::new(
            revision(4),
            ContractFingerprint::new(5),
            GenomicInterval::new(ContigId::new(0), 0, 10).unwrap(),
            FilteredRecordProvenance {
                records_seen: 10,
                eligible_records: 9,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap_err();
        assert_eq!(
            error,
            CoverageError::IncompleteProvenance {
                seen: 10,
                accounted: 9
            }
        );
    }

    #[test]
    fn coverage_union_merges_only_touching_compatible_intervals() {
        let mut coverage = CertifiedCoverageSet::new(revision(4), ContractFingerprint::new(5));
        coverage.insert(certificate(20, 30)).unwrap();
        coverage.insert(certificate(0, 10)).unwrap();
        coverage.insert(certificate(10, 20)).unwrap();

        assert_eq!(
            coverage.intervals(),
            &[GenomicInterval::new(ContigId::new(0), 0, 30).unwrap()]
        );
        assert!(coverage.covers(GenomicInterval::new(ContigId::new(0), 5, 25).unwrap()));
        assert_eq!(
            coverage.exact_provenance(GenomicInterval::new(ContigId::new(0), 0, 30).unwrap()),
            Some(FilteredRecordProvenance {
                records_seen: 30,
                eligible_records: 21,
                user_filter: 9,
                ..FilteredRecordProvenance::default()
            })
        );
        assert_eq!(
            coverage.exact_provenance(GenomicInterval::new(ContigId::new(0), 5, 25).unwrap()),
            None
        );
    }

    #[test]
    fn conflicting_provenance_for_the_same_source_interval_is_rejected() {
        let mut coverage = CertifiedCoverageSet::new(revision(4), ContractFingerprint::new(5));
        coverage.insert(certificate(0, 10)).unwrap();
        let conflicting = CompleteCoverage::new(
            revision(4),
            ContractFingerprint::new(5),
            GenomicInterval::new(ContigId::new(0), 0, 10).unwrap(),
            FilteredRecordProvenance {
                records_seen: 10,
                eligible_records: 8,
                user_filter: 2,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();

        assert_eq!(
            coverage.insert(conflicting),
            Err(CoverageError::ConflictingCertificate(
                GenomicInterval::new(ContigId::new(0), 0, 10).unwrap()
            ))
        );
    }
}
