use std::{mem::size_of, num::NonZeroUsize, sync::Arc};

use rgv_alignment_scene::{
    AlignmentHandle, AlignmentSourceRevision, AllocationClass, AllocationLedger,
    CanonicalRecordChunk, CapabilityRequirement, ChunkError, CompleteCoverage, ContractFingerprint,
    CoverageError, EvidenceChunk, EvidenceChunkError, FilteredRecordProvenance, LedgerError,
    PayloadContract, PayloadSidecar, PayloadSidecarError, RecordContract, ReservationKind,
    SourceChunkId, TransientResourceClass,
};
use rgv_core::{GenomicInterval, OverlappingMatePolicy};
use rgv_hts::{
    AlignmentBatch, AlignmentError, AlignmentPayloadBatch, AlignmentPayloadError, AlignmentSource,
    IndividualReadStatus, LocalAlignmentSource, PositionedAlignmentBlock,
    PositionedAlignmentStreamCheckpoint, QueryDetail, QueryOptions, QueryStats,
};
use thiserror::Error;

use crate::{
    AlignmentSourceIntentSpec, CanonicalAlignmentSourceBackend, CanonicalDecodeCheckpoint,
    SourceWork,
};

/// Non-discardable controls which do not change physical-record eligibility. Filter or source
/// changes require a new actor/store because they change stable authority contracts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalBamSourceControl {
    SetCompleteReadLimit(NonZeroUsize),
    SetCancellationChunk(NonZeroUsize),
}

/// Production adapter from the resumable local BAM decoder to canonical immutable evidence.
///
/// One instance owns one mutable indexed HTS reader. It never certifies a partial transaction and
/// it returns to the actor after every physical owner block, including blocks containing only
/// filtered records.
pub struct LocalBamCanonicalBackend {
    source: LocalAlignmentSource,
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    options: QueryOptions,
}

impl LocalBamCanonicalBackend {
    pub fn new(
        source: LocalAlignmentSource,
        mut options: QueryOptions,
    ) -> Result<Self, LocalBamCanonicalBackendError> {
        let stamp = source
            .source_stamp()
            .ok_or(LocalBamCanonicalBackendError::MissingSourceRevision)?;
        let source_revision = AlignmentSourceRevision::from(stamp);
        let record_contract = record_contract_fingerprint(&options);

        // Exact summaries are scene-owned independent products. Source evidence decoding only
        // retains the payload requested by the active intent.
        options.collect_alleles = false;
        options.collect_modifications = false;
        options.overlapping_mate_policy = OverlappingMatePolicy::CountBoth;
        options.include_reference = false;
        Ok(Self {
            source,
            source_revision,
            record_contract,
            options,
        })
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
    pub fn intent_spec(
        &self,
        target: GenomicInterval,
        payload: PayloadContract,
    ) -> AlignmentSourceIntentSpec {
        AlignmentSourceIntentSpec::new(self.source_revision, self.record_contract, payload, target)
    }

    fn validate_work(&self, work: &SourceWork) -> Result<(), LocalBamCanonicalBackendError> {
        if work.intent().spec().source_revision() != self.source_revision {
            return Err(LocalBamCanonicalBackendError::SourceRevisionMismatch);
        }
        if work.intent().spec().record_contract() != self.record_contract {
            return Err(LocalBamCanonicalBackendError::RecordContractMismatch);
        }
        validate_payload_contract(work.intent().spec().payload())
    }

    fn options_for(
        &self,
        payload: &PayloadContract,
    ) -> Result<QueryOptions, LocalBamCanonicalBackendError> {
        validate_payload_contract(payload)?;
        let retain_sequence = payload.sequence() == CapabilityRequirement::Required;
        let mut options = self.options.clone();
        options.detail = if payload.all_auxiliary_tags() {
            QueryDetail::Full
        } else if retain_sequence {
            QueryDetail::Sequence
        } else {
            QueryDetail::Layout
        };
        options.layout_requires_sequence = retain_sequence;
        options.retained_auxiliary_tags = payload.auxiliary_tags().to_vec();
        Ok(options)
    }

    fn begin_or_retarget(
        &mut self,
        work: &SourceWork,
    ) -> Result<(), LocalBamCanonicalBackendError> {
        self.validate_work(work)?;
        let options = self.options_for(work.intent().spec().payload())?;
        self.source
            .begin_positioned_stream(work.intent().spec().target(), options)?;
        Ok(())
    }

    fn pack_block(
        &self,
        work: &SourceWork,
        block: PositionedAlignmentBlock,
        ledger: &AllocationLedger,
    ) -> Result<Arc<EvidenceChunk>, LocalBamCanonicalBackendError> {
        let mut decoded = AlignmentBatch::default();
        for record in block.records {
            decoded
                .push_with_source_position(&record.record, record.source_position)
                .map_err(AlignmentError::BatchOverflow)?;
        }

        // The decoded batch is bounded decoder scratch. Before creating any retained scene
        // allocation, reserve conservative maxima for core, payload, and overlap storage. Core
        // and payload each retain a strict subset of the decoded packed columns.
        let decoded_bytes = decoded.estimated_bytes();
        let core_maximum = decoded_bytes
            .saturating_add(decoded.len().saturating_mul(size_of::<AlignmentHandle>()))
            .saturating_mul(2);
        let mut core_reservation = ledger.reserve(
            AllocationClass::CoreChunk,
            core_maximum,
            ReservationKind::Normal,
        )?;
        let overlap_maximum = decoded.len().saturating_mul(size_of::<u64>());
        let mut overlap_reservation = ledger.reserve(
            AllocationClass::OverlapIndex,
            overlap_maximum,
            ReservationKind::Normal,
        )?;
        let payload_contract = work.intent().spec().payload();
        let retain_payload = payload_is_required(payload_contract);
        let mut payload_reservation = retain_payload
            .then(|| {
                ledger.reserve(
                    AllocationClass::PayloadSidecar,
                    decoded_bytes.saturating_mul(2),
                    ReservationKind::Normal,
                )
            })
            .transpose()?;

        let core = CanonicalRecordChunk::new(
            self.source_revision,
            SourceChunkId::BamBlock {
                compressed_offset: block.compressed_offset,
            },
            Arc::new(decoded.copy_core().map_err(AlignmentError::BatchOverflow)?),
            self.record_contract,
        )?;
        let footprint = EvidenceChunk::footprint(&core);
        let core_allocation = core_reservation.register(footprint.core_bytes, work.id().get())?;
        let overlap_allocation =
            overlap_reservation.register(footprint.overlap_index_bytes, work.id().get())?;

        if let Some(payload_reservation) = payload_reservation.as_mut() {
            let payload = Arc::new(AlignmentPayloadBatch::from_batch(
                &decoded,
                payload_contract.sequence() == CapabilityRequirement::Required,
                payload_contract.qualities() == CapabilityRequirement::Required,
                (!payload_contract.all_auxiliary_tags())
                    .then_some(payload_contract.auxiliary_tags()),
            )?);
            let payload_bytes = payload.estimated_bytes();
            let payload_allocation =
                payload_reservation.register(payload_bytes, work.id().get())?;
            let payload = Arc::new(PayloadSidecar::new(
                &core,
                payload_contract.clone(),
                payload,
                payload_allocation,
            )?);
            Ok(Arc::new(EvidenceChunk::new_with_payload(
                core,
                core_allocation,
                overlap_allocation,
                payload,
            )?))
        } else {
            Ok(Arc::new(EvidenceChunk::new(
                core,
                core_allocation,
                overlap_allocation,
            )?))
        }
    }

    fn coverage(
        &self,
        work: &SourceWork,
        interval: GenomicInterval,
        stats: QueryStats,
    ) -> Result<CompleteCoverage, LocalBamCanonicalBackendError> {
        if interval != work.intent().spec().target() {
            return Err(LocalBamCanonicalBackendError::CompletionTargetMismatch {
                expected: work.intent().spec().target(),
                actual: interval,
            });
        }
        let provenance = FilteredRecordProvenance {
            records_seen: count_to_u64(stats.records_seen)?,
            eligible_records: count_to_u64(stats.records_eligible)?,
            unmapped: count_to_u64(stats.records_filtered_unmapped)?,
            secondary: count_to_u64(stats.records_filtered_secondary)?,
            supplementary: count_to_u64(stats.records_filtered_supplementary)?,
            user_filter: count_to_u64(stats.records_filtered_user)?,
        };
        Ok(CompleteCoverage::new(
            self.source_revision,
            self.record_contract,
            interval,
            provenance,
        )?)
    }
}

impl CanonicalAlignmentSourceBackend for LocalBamCanonicalBackend {
    type Control = LocalBamSourceControl;
    type Error = LocalBamCanonicalBackendError;

    fn allocation_pressure(error: &Self::Error) -> Option<LedgerError> {
        match error {
            LocalBamCanonicalBackendError::Ledger(
                pressure @ (LedgerError::BudgetExceeded { .. }
                | LedgerError::TransientBudgetExceeded { .. }
                | LedgerError::ReplacementTransitionActive { .. }),
            ) => Some(*pressure),
            _ => None,
        }
    }

    fn control_invalidates_read_refusals(control: &Self::Control) -> bool {
        matches!(control, LocalBamSourceControl::SetCompleteReadLimit(_))
    }

    fn apply_control(&mut self, control: Self::Control) -> Result<(), Self::Error> {
        match control {
            LocalBamSourceControl::SetCompleteReadLimit(limit) => {
                self.options.max_records = limit;
            }
            LocalBamSourceControl::SetCancellationChunk(chunk) => {
                self.options.cancellation_chunk = chunk;
            }
        }
        Ok(())
    }

    fn begin_work(
        &mut self,
        work: &SourceWork,
        _ledger: &AllocationLedger,
    ) -> Result<(), Self::Error> {
        self.begin_or_retarget(work)
    }

    fn next_checkpoint(
        &mut self,
        work: &SourceWork,
        ledger: &AllocationLedger,
    ) -> Result<CanonicalDecodeCheckpoint, Self::Error> {
        self.validate_work(work)?;
        match self.source.next_positioned_stream_checkpoint(&|| true)? {
            PositionedAlignmentStreamCheckpoint::Block(block) => {
                let genomic_span = block
                    .genomic_span
                    .unwrap_or_else(|| work.intent().spec().target());
                if block.records.is_empty() {
                    Ok(CanonicalDecodeCheckpoint::Progress { genomic_span })
                } else {
                    // The returned normalized owner block remains live while we create packed
                    // core, overlap, and optional payload derivatives. Reserve their concurrent
                    // logical capacities before any of those duplicate allocations. HTSlib's
                    // opaque internal decode buffers are reported separately from this owned
                    // Rust working set.
                    let _decoder_scratch = ledger.reserve_transient(
                        TransientResourceClass::DecoderScratch,
                        block.estimated_bytes().saturating_mul(6),
                        ReservationKind::Normal,
                    )?;
                    Ok(CanonicalDecodeCheckpoint::Chunk {
                        chunk: self.pack_block(work, block, ledger)?,
                        genomic_span,
                    })
                }
            }
            PositionedAlignmentStreamCheckpoint::Complete(completion) => {
                let status = completion.stats.individual_reads;
                let limit = completion.stats.complete_read_limit;
                let coverage = self.coverage(work, completion.interval, completion.stats)?;
                match status {
                    IndividualReadStatus::Complete => {
                        Ok(CanonicalDecodeCheckpoint::Complete(coverage))
                    }
                    IndividualReadStatus::TooDense { .. } => {
                        Ok(CanonicalDecodeCheckpoint::Refused {
                            coverage,
                            complete_read_limit: limit,
                        })
                    }
                }
            }
            PositionedAlignmentStreamCheckpoint::Cancelled(_) => {
                Err(LocalBamCanonicalBackendError::UnexpectedDecoderCancellation)
            }
        }
    }

    fn retarget(&mut self, work: &SourceWork) -> Result<(), Self::Error> {
        self.begin_or_retarget(work)
    }

    fn cancel(&mut self, _work: &SourceWork) {
        self.source.cancel_positioned_stream();
    }
}

fn payload_is_required(payload: &PayloadContract) -> bool {
    payload.sequence() == CapabilityRequirement::Required
        || payload.qualities() == CapabilityRequirement::Required
        || payload.all_auxiliary_tags()
        || !payload.auxiliary_tags().is_empty()
        || payload.decode_reference().is_some()
}

fn validate_payload_contract(
    payload: &PayloadContract,
) -> Result<(), LocalBamCanonicalBackendError> {
    if payload.qualities() == CapabilityRequirement::Required
        && payload.sequence() != CapabilityRequirement::Required
    {
        return Err(LocalBamCanonicalBackendError::QualitiesRequireSequence);
    }
    if payload.decode_reference().is_some() {
        return Err(LocalBamCanonicalBackendError::DecodeReferenceUnsupportedForBam);
    }
    Ok(())
}

fn record_contract_fingerprint(options: &QueryOptions) -> ContractFingerprint {
    RecordContract::new(
        ContractFingerprint::new(options.filter.fingerprint()),
        options.include_secondary,
        options.include_supplementary,
    )
    .fingerprint()
}

fn count_to_u64(value: usize) -> Result<u64, LocalBamCanonicalBackendError> {
    u64::try_from(value).map_err(|_| LocalBamCanonicalBackendError::RecordCountOverflow)
}

#[derive(Debug, Error)]
pub enum LocalBamCanonicalBackendError {
    #[error("the alignment source has no validated immutable revision")]
    MissingSourceRevision,
    #[error("source work belongs to a different alignment source revision")]
    SourceRevisionMismatch,
    #[error("source work uses a different physical-record contract")]
    RecordContractMismatch,
    #[error("quality payload capability requires sequence capability")]
    QualitiesRequireSequence,
    #[error("a BAM payload cannot require a CRAM decode-reference revision")]
    DecodeReferenceUnsupportedForBam,
    #[error("the decoder cancelled despite the actor retaining active work")]
    UnexpectedDecoderCancellation,
    #[error("an alignment record count cannot be represented as u64")]
    RecordCountOverflow,
    #[error("source completion target {actual:?} did not match active target {expected:?}")]
    CompletionTargetMismatch {
        expected: GenomicInterval,
        actual: GenomicInterval,
    },
    #[error(transparent)]
    Alignment(#[from] AlignmentError),
    #[error(transparent)]
    Payload(#[from] AlignmentPayloadError),
    #[error(transparent)]
    Chunk(#[from] ChunkError),
    #[error(transparent)]
    PayloadSidecar(#[from] PayloadSidecarError),
    #[error(transparent)]
    EvidenceChunk(#[from] EvidenceChunkError),
    #[error(transparent)]
    Coverage(#[from] CoverageError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[cfg(test)]
mod tests {
    use std::{
        num::{NonZeroU64, NonZeroUsize},
        path::Path,
        sync::Arc,
    };

    use rgv_alignment_scene::{
        AlleleSummaryDetail, AllocationLedger, CapabilityRequirement, EvidenceStore,
        FilteredRecordProvenance, PayloadContract, ReservationKind, SummaryContract,
    };
    use rgv_core::{ContigId, OverlappingMatePolicy};
    use rgv_hts::{AlignmentOpenOptions, QueryDetail, SequenceView};

    use super::*;
    use crate::{AlignmentSourceActor, AlignmentSourceActorEvent};

    fn no_payload() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            std::iter::empty(),
            None,
        )
    }

    fn fixture_backend(complete_read_limit: usize) -> (LocalBamCanonicalBackend, GenomicInterval) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        let source = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let backend = LocalBamCanonicalBackend::new(
            source,
            QueryOptions {
                max_records: NonZeroUsize::new(complete_read_limit).unwrap(),
                ..QueryOptions::default()
            },
        )
        .unwrap();
        (
            backend,
            GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap(),
        )
    }

    #[test]
    fn production_local_bam_backend_admits_exact_complete_evidence() {
        let (backend, target) = fixture_backend(100);
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let intent = backend.intent_spec(target, no_payload());
        let ledger = AllocationLedger::new(4 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        let sequence = actor.submit_foreground(intent).unwrap();
        let mut completion = None;
        for _ in 0..32 {
            if let Some(event) = actor.pump_once().unwrap()
                && let AlignmentSourceActorEvent::AcquisitionCompleted {
                    publish,
                    admitted_revision,
                    ..
                } = event
            {
                completion = Some((publish, admitted_revision));
                break;
            }
        }

        assert_eq!(completion.unwrap().0, Some(sequence));
        assert_eq!(actor.store().acquisition_count(), 1);
        assert!(actor.store().chunk_count() >= 1);
        let candidate = actor
            .store_mut()
            .prepare(target, no_payload(), ReservationKind::Normal)
            .unwrap();
        assert!(candidate.root().coverage().covers(target));
        assert_eq!(candidate.root().record_count_upper_bound(), 13);
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.reserved, 0);
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::DecoderScratch
                && usage.current == 0
                && usage.high_water > 0
        }));
    }

    #[test]
    fn production_decoder_scratch_refusal_publishes_no_partial_chunk() {
        let (backend, target) = fixture_backend(100);
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let intent = backend.intent_spec(target, no_payload());
        let ledger = AllocationLedger::new_with_transient_budget(4 << 20, 0, 1);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        actor.submit_foreground(intent).unwrap();
        assert!(matches!(
            actor.pump_once().unwrap(),
            Some(AlignmentSourceActorEvent::WorkStarted(_))
        ));
        let error = actor.pump_once().unwrap_err();
        assert!(matches!(
            error,
            crate::AlignmentSourceActorError::Work {
                source: crate::AlignmentSourceWorkError::Backend(
                    LocalBamCanonicalBackendError::Ledger(LedgerError::TransientBudgetExceeded {
                        class: TransientResourceClass::DecoderScratch,
                        ..
                    })
                ),
                ..
            }
        ));
        assert_eq!(actor.store().chunk_count(), 0);
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert_eq!(snapshot.reserved, 0);
    }

    #[test]
    fn production_scene_population_and_summaries_match_a_fresh_indexed_query() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        let target = GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(100).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            overlapping_mate_policy: OverlappingMatePolicy::CountOnce,
            ..QueryOptions::default()
        };

        let mut oracle_source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let contig_length = oracle_source
            .reference_dictionary()
            .get(target.contig())
            .unwrap()
            .length();
        let oracle = oracle_source
            .query(target, options.clone(), &|| true)
            .unwrap();
        assert!(oracle.individual_reads_complete());

        let source = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let backend = LocalBamCanonicalBackend::new(source, options.clone()).unwrap();
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let payload = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let intent = backend.intent_spec(target, payload.clone());
        let ledger = AllocationLedger::new(64 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        actor.submit_foreground(intent).unwrap();
        for _ in 0..64 {
            if matches!(
                actor.pump_once().unwrap(),
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
            ) {
                break;
            }
        }
        let candidate = actor
            .store_mut()
            .prepare(target, payload, ReservationKind::Normal)
            .unwrap();
        let evidence = Arc::clone(candidate.root());
        drop(candidate);
        let selection = evidence.select(target).unwrap();
        let mut actual_records = (0..selection.len())
            .map(|index| {
                (
                    selection.records()[index].handle.source_position(),
                    selection.materialize_retained_record(index).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let mut expected_records = (0..oracle.batch.len())
            .map(|index| {
                (
                    oracle
                        .batch
                        .source_position(index)
                        .expect("indexed BAM oracle records carry physical positions"),
                    oracle.batch.materialize_record(index).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        // The evidence selection intentionally uses genomic presentation order, while the
        // independent indexed query retains physical BAM order. Compare the exact physical
        // population and full payload by their common source-position authority instead of
        // accidentally making either ordering policy part of the evidence contract.
        actual_records.sort_unstable_by_key(|(position, _)| *position);
        expected_records.sort_unstable_by_key(|(position, _)| *position);
        assert_eq!(actual_records, expected_records);
        assert_eq!(
            selection
                .records()
                .iter()
                .map(|record| record.handle)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            expected_records.len()
        );
        drop(selection);

        let summary_contract = SummaryContract {
            overlapping_mate_policy: options.overlapping_mate_policy,
            minimum_base_quality: options.minimum_base_quality,
            minimum_junction_flank: options.minimum_junction_flank,
            allele_detail: AlleleSummaryDetail::QualityWeighted,
            collect_modifications: false,
        };
        let summaries = actor
            .store_mut()
            .prepare_exact_summaries(
                &evidence,
                target,
                NonZeroU64::new(1).unwrap(),
                contig_length,
                summary_contract,
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        let root = summaries.root();
        let projected_depth = root.project_depth_bins(target, 2).unwrap();
        assert_eq!(projected_depth.len(), oracle.pileup.bins.len());
        for (actual, expected) in projected_depth.iter().zip(&oracle.pileup.bins) {
            assert_eq!(actual.depth, expected.depth);
            assert_eq!(actual.forward_depth, expected.forward_depth);
            assert_eq!(actual.reverse_depth, expected.reverse_depth);
            assert_eq!(actual.mapping_quality_sum, expected.mapping_quality_sum);
            assert_eq!(
                actual.mapping_quality_observations,
                expected.mapping_quality_observations
            );
        }
        for position in target.start()..target.end() {
            let actual = root.allele_site(position).unwrap().unwrap();
            let expected = oracle.pileup.bin_at(target, position).unwrap().1;
            assert_eq!(actual.alleles, expected.alleles, "position {position}");
            assert_eq!(actual.forward_alleles, expected.forward_alleles);
            assert_eq!(actual.reverse_alleles, expected.reverse_alleles);
            assert_eq!(actual.quality_weighted, expected.quality_weighted);
            assert_eq!(actual.base_quality_sum, expected.base_quality_sum);
            assert_eq!(
                actual.base_quality_observations,
                expected.base_quality_observations
            );
        }
        assert_eq!(root.junctions(target).unwrap(), oracle.junctions.junctions);
        assert_eq!(root.pair_summary(target).unwrap(), oracle.pairs);
        assert_eq!(
            root.filter_provenance(target).unwrap(),
            FilteredRecordProvenance {
                records_seen: oracle.stats.records_seen as u64,
                eligible_records: oracle.stats.records_eligible as u64,
                unmapped: oracle.stats.records_filtered_unmapped as u64,
                secondary: oracle.stats.records_filtered_secondary as u64,
                supplementary: oracle.stats.records_filtered_supplementary as u64,
                user_filter: oracle.stats.records_filtered_user as u64,
            }
        );
        assert_eq!(ledger.snapshot().unwrap().reserved, 0);
    }

    #[test]
    fn production_local_bam_backend_refuses_dense_reads_without_false_acquisition() {
        let (backend, target) = fixture_backend(1);
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let intent = backend.intent_spec(target, no_payload());
        let ledger = AllocationLedger::new(4 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        let sequence = actor.submit_foreground(intent).unwrap();
        let mut refusal = None;
        for _ in 0..32 {
            if let Some(event) = actor.pump_once().unwrap()
                && let AlignmentSourceActorEvent::AcquisitionRefused {
                    coverage,
                    complete_read_limit,
                    publish,
                    ..
                } = event
            {
                refusal = Some((coverage, complete_read_limit, publish));
                break;
            }
        }

        let (coverage, limit, publish) = refusal.expect("dense fixture should be refused");
        assert_eq!(coverage.provenance().eligible_records, 13);
        assert_eq!(limit, 1);
        assert_eq!(publish, Some(sequence));
        assert_eq!(actor.store().acquisition_count(), 0);
        assert!(actor.store().chunk_count() <= 1);
        assert_eq!(ledger.snapshot().unwrap().reserved, 0);
    }

    #[test]
    fn production_local_bam_backend_keeps_requested_payload_out_of_core_identity() {
        let (backend, target) = fixture_backend(100);
        let revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"RG"],
            None,
        );
        let intent = backend.intent_spec(target, payload.clone());
        let ledger = AllocationLedger::new(4 << 20, 0);
        let store = EvidenceStore::new(revision, record_contract, ledger.clone());
        let mut actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        actor.submit_foreground(intent).unwrap();
        for _ in 0..32 {
            if matches!(
                actor.pump_once().unwrap(),
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
            ) {
                break;
            }
        }

        let candidate = actor
            .store_mut()
            .prepare(target, payload, ReservationKind::Normal)
            .unwrap();
        let selected = candidate.root().select(target).unwrap();
        assert_eq!(selected.len(), 13);
        assert!((0..selected.len()).all(|index| matches!(
            selected.record_view(index).unwrap().sequence(),
            SequenceView::NotLoaded
        )));
        assert!((0..selected.len()).all(|index| matches!(
            selected.sequence(index),
            Some(SequenceView::Present {
                qualities: Some(_),
                ..
            })
        )));
        assert!(ledger.snapshot().unwrap().records.iter().any(|allocation| {
            allocation.class == AllocationClass::PayloadSidecar && allocation.charged_capacity > 0
        }));
    }
}
