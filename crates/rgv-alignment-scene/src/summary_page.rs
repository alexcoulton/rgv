use std::{mem::size_of, sync::Arc};

use rgv_core::GenomicInterval;
use rgv_hts::{JunctionEvidence, ModificationBin, ModificationSeriesKey, PairSummary, PileupBin};
use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    AnnotationRevision, CanonicalGenomicTile, ContractFingerprint, EvidenceRevision,
    ExactSummaryQueryError, ExactSummaryRoot, JunctionAnnotationIndex, JunctionAnnotationStatus,
    LedgerError, ReservationKind, SummaryRevision, TransientResourceClass,
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SummaryPageFamily {
    Depth,
    Alleles,
    Junctions,
    Modifications,
    Pairs,
    FilterProvenance,
}

/// Camera-independent identity for one exact derived-lane semantic page.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SummaryPageKey {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    summary_revision: SummaryRevision,
    summary_contract: ContractFingerprint,
    annotation_revision: Option<AnnotationRevision>,
    tile: CanonicalGenomicTile,
    family: SummaryPageFamily,
}

impl SummaryPageKey {
    #[must_use]
    pub fn new(
        root: &ExactSummaryRoot,
        tile: CanonicalGenomicTile,
        family: SummaryPageFamily,
    ) -> Self {
        Self::new_with_annotation(root, tile, family, None)
    }

    #[must_use]
    pub fn new_with_annotation(
        root: &ExactSummaryRoot,
        tile: CanonicalGenomicTile,
        family: SummaryPageFamily,
        annotation: Option<&JunctionAnnotationIndex>,
    ) -> Self {
        Self {
            source_revision: root.source_revision(),
            evidence_revision: root.evidence_revision(),
            summary_revision: root.revision(),
            summary_contract: root.contract().fingerprint(),
            annotation_revision: (family == SummaryPageFamily::Junctions)
                .then(|| annotation.map(JunctionAnnotationIndex::revision))
                .flatten(),
            tile,
            family,
        }
    }

    #[must_use]
    pub const fn source_revision(self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn evidence_revision(self) -> EvidenceRevision {
        self.evidence_revision
    }

    #[must_use]
    pub const fn summary_revision(self) -> SummaryRevision {
        self.summary_revision
    }

    #[must_use]
    pub const fn annotation_revision(self) -> Option<AnnotationRevision> {
        self.annotation_revision
    }

    #[must_use]
    pub const fn tile(self) -> CanonicalGenomicTile {
        self.tile
    }

    #[must_use]
    pub const fn family(self) -> SummaryPageFamily {
        self.family
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummarySemanticComponent {
    Depth {
        depth: u32,
        forward_depth: u32,
        reverse_depth: u32,
        mapping_quality_sum: u64,
        mapping_quality_observations: u32,
    },
    Alleles(PileupBin),
    Junction(AnnotatedJunctionSemantic),
    Modification {
        series: ModificationSeriesKey,
        bin: ModificationBin,
    },
    PairCandidate {
        handle: crate::AlignmentHandle,
        score: u64,
        template_length: u64,
    },
    PairStatistics(PairSummary),
    FilterProvenance(crate::CompleteCoverage),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnnotatedJunctionSemantic {
    pub evidence: JunctionEvidence,
    /// `None` means no annotation authority was requested for this scene. `Some(Unknown)` means
    /// an authority exists but does not certify this junction's interval.
    pub annotation: Option<JunctionAnnotationStatus>,
}

/// Tile-local raw summary fact. Display transforms, thresholds, scale maxima, labels, and camera
/// pixels are projection state and never alter this exact semantic authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SummarySemanticPrimitive {
    pub start_offset: u64,
    pub end_offset: u64,
    pub component: SummarySemanticComponent,
}

#[derive(Debug)]
pub struct SummarySemanticPage {
    key: SummaryPageKey,
    interval: GenomicInterval,
    primitives: Arc<[SummarySemanticPrimitive]>,
    allocation: AllocationLease,
}

impl SummarySemanticPage {
    pub fn build(
        key: SummaryPageKey,
        root: &Arc<ExactSummaryRoot>,
        annotation: Option<&JunctionAnnotationIndex>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, SummarySemanticPageError> {
        validate_key(key, root, annotation)?;
        let interval = root
            .depth_tiles()
            .iter()
            .find(|tile| tile.tile() == key.tile)
            .map(|tile| tile.depth().interval())
            .ok_or(SummarySemanticPageError::MissingCanonicalTile)?;
        let maximum_primitives = maximum_primitives(root, key, interval)?;
        let maximum_bytes = page_bytes(maximum_primitives);
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::SummaryPageBuilder,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut reservation = ledger.reserve(
            AllocationClass::SemanticPage,
            maximum_bytes,
            reservation_kind,
        )?;
        let mut primitives = Vec::with_capacity(maximum_primitives);
        match key.family {
            SummaryPageFamily::Depth => emit_depth(root, key.tile, interval, &mut primitives),
            SummaryPageFamily::Alleles => emit_alleles(root, key.tile, interval, &mut primitives),
            SummaryPageFamily::Junctions => {
                emit_junctions(root, interval, annotation, &mut primitives)?;
            }
            SummaryPageFamily::Modifications => {
                emit_modifications(root, key.tile, interval, &mut primitives);
            }
            SummaryPageFamily::Pairs => emit_pairs(root, key.tile, interval, &mut primitives)?,
            SummaryPageFamily::FilterProvenance => {
                emit_filter_provenance(root, key.tile, interval, &mut primitives)?;
            }
        }
        primitives.sort_by_key(|primitive| (primitive.start_offset, primitive.end_offset));
        primitives.shrink_to_fit();
        let retained_bytes = page_bytes(primitives.len());
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            key,
            interval,
            primitives: primitives.into(),
            allocation,
        }))
    }

    #[must_use]
    pub const fn key(&self) -> SummaryPageKey {
        self.key
    }

    #[must_use]
    pub const fn interval(&self) -> GenomicInterval {
        self.interval
    }

    #[must_use]
    pub fn primitives(&self) -> &[SummarySemanticPrimitive] {
        &self.primitives
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }
}

fn validate_key(
    key: SummaryPageKey,
    root: &ExactSummaryRoot,
    annotation: Option<&JunctionAnnotationIndex>,
) -> Result<(), SummarySemanticPageError> {
    if key.source_revision != root.source_revision()
        || key.evidence_revision != root.evidence_revision()
        || key.summary_revision != root.revision()
        || key.summary_contract != root.contract().fingerprint()
        || key.annotation_revision
            != (key.family == SummaryPageFamily::Junctions)
                .then(|| annotation.map(JunctionAnnotationIndex::revision))
                .flatten()
    {
        return Err(SummarySemanticPageError::AuthorityMismatch);
    }
    Ok(())
}

fn maximum_primitives(
    root: &ExactSummaryRoot,
    key: SummaryPageKey,
    interval: GenomicInterval,
) -> Result<usize, SummarySemanticPageError> {
    Ok(match key.family {
        SummaryPageFamily::Depth => root
            .depth_tiles()
            .iter()
            .find(|tile| tile.tile() == key.tile)
            .map_or(0, |tile| tile.depth().runs().len()),
        SummaryPageFamily::Alleles => {
            usize::try_from(interval.len()).map_err(|_| SummarySemanticPageError::TileTooWide)?
        }
        SummaryPageFamily::Junctions => root.junction_count(interval)?,
        SummaryPageFamily::Modifications => root
            .modification_tiles()
            .iter()
            .find(|tile| tile.tile() == key.tile)
            .map_or(0, |tile| {
                tile.summary()
                    .series
                    .len()
                    .saturating_mul(tile.summary().bin_count())
            }),
        SummaryPageFamily::Pairs => root
            .pair_tiles()
            .iter()
            .find(|tile| tile.tile() == key.tile)
            .map_or(1, |tile| tile.candidates().len().saturating_add(1)),
        SummaryPageFamily::FilterProvenance => root
            .filter_tiles()
            .iter()
            .find(|summary| summary.tile() == key.tile)
            .map_or(0, |summary| summary.provenances().len()),
    })
}

fn emit_depth(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) {
    let Some(summary) = root
        .depth_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
    else {
        return;
    };
    output.extend(
        summary
            .depth()
            .runs()
            .iter()
            .map(|run| SummarySemanticPrimitive {
                start_offset: run.start - interval.start(),
                end_offset: run.end - interval.start(),
                component: SummarySemanticComponent::Depth {
                    depth: run.depth,
                    forward_depth: run.forward_depth,
                    reverse_depth: run.reverse_depth,
                    mapping_quality_sum: run.mapping_quality_sum,
                    mapping_quality_observations: run.mapping_quality_observations,
                },
            }),
    );
}

fn emit_alleles(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) {
    let Some(summary) = root
        .allele_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
    else {
        return;
    };
    output.extend(
        summary
            .sites()
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, bin)| {
                let observations = bin
                    .alleles
                    .reference_observations()
                    .saturating_add(bin.alleles.insertion);
                (observations > 0).then(|| SummarySemanticPrimitive {
                    start_offset: index as u64,
                    end_offset: (index as u64).saturating_add(1).min(interval.len()),
                    component: SummarySemanticComponent::Alleles(bin),
                })
            }),
    );
}

fn emit_junctions(
    root: &ExactSummaryRoot,
    interval: GenomicInterval,
    annotation: Option<&JunctionAnnotationIndex>,
    output: &mut Vec<SummarySemanticPrimitive>,
) -> Result<(), SummarySemanticPageError> {
    root.for_each_junction(interval, |junction| {
        let start = junction.start.max(interval.start());
        let end = junction.end.min(interval.end());
        if start < end {
            output.push(SummarySemanticPrimitive {
                start_offset: start - interval.start(),
                end_offset: end - interval.start(),
                component: SummarySemanticComponent::Junction(AnnotatedJunctionSemantic {
                    evidence: junction,
                    annotation: annotation
                        .map(|index| index.status(interval.contig(), junction.start, junction.end)),
                }),
            });
        }
    })?;
    Ok(())
}

fn emit_modifications(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) {
    let Some(summary) = root
        .modification_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
    else {
        return;
    };
    for series in &summary.summary().series {
        for (index, bin) in series.bins.iter().copied().enumerate() {
            if bin.canonical_observations == 0 && bin.modified_calls == 0 {
                continue;
            }
            output.push(SummarySemanticPrimitive {
                start_offset: index as u64,
                end_offset: (index as u64).saturating_add(1).min(interval.len()),
                component: SummarySemanticComponent::Modification {
                    series: series.key,
                    bin,
                },
            });
        }
    }
}

fn emit_pairs(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) -> Result<(), SummarySemanticPageError> {
    let Some(summary) = root
        .pair_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
    else {
        return Ok(());
    };
    output.extend(summary.candidates().iter().map(|candidate| {
        SummarySemanticPrimitive {
            start_offset: candidate.record_start.saturating_sub(interval.start()),
            end_offset: candidate
                .record_end
                .saturating_sub(interval.start())
                .min(interval.len()),
            component: SummarySemanticComponent::PairCandidate {
                handle: candidate.handle,
                score: candidate.score,
                template_length: candidate.template_length,
            },
        }
    }));
    output.push(SummarySemanticPrimitive {
        start_offset: 0,
        end_offset: interval.len(),
        component: SummarySemanticComponent::PairStatistics(root.pair_summary(interval)?),
    });
    Ok(())
}

fn emit_filter_provenance(
    root: &ExactSummaryRoot,
    tile: CanonicalGenomicTile,
    interval: GenomicInterval,
    output: &mut Vec<SummarySemanticPrimitive>,
) -> Result<(), SummarySemanticPageError> {
    let provenances = root
        .filter_tiles()
        .iter()
        .find(|summary| summary.tile() == tile)
        .ok_or(SummarySemanticPageError::MissingCanonicalTile)?
        .provenances();
    for provenance in provenances.iter().copied() {
        output.push(SummarySemanticPrimitive {
            start_offset: 0,
            end_offset: interval.len(),
            component: SummarySemanticComponent::FilterProvenance(provenance),
        });
    }
    Ok(())
}

const fn page_bytes(primitives: usize) -> usize {
    size_of::<SummarySemanticPage>()
        .saturating_add(primitives.saturating_mul(size_of::<SummarySemanticPrimitive>()))
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SummarySemanticPageError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Query(#[from] ExactSummaryQueryError),
    #[error("summary page key does not match its exact summary authority")]
    AuthorityMismatch,
    #[error("summary page canonical tile is absent from the exact summary root")]
    MissingCanonicalTile,
    #[error("summary page canonical tile is too wide for this platform")]
    TileTooWide,
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64};

    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId,
        OverlappingMatePolicy, ReadStackingMode,
    };
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, SequenceData,
        TemplateLengthBounds,
    };

    use super::*;
    use crate::{
        AlignmentPageKey, AlignmentPresentationScene, AlignmentPresentationStore,
        AlignmentRowAtlas, AlignmentSceneSnapshot, AlignmentSceneSnapshotError,
        AlignmentSemanticPage, AlignmentSourceRevision, AlleleSummaryDetail, AllocationClass,
        CanonicalRowBlock, CapabilityRequirement, CompleteCoverage, EvidenceAcquisition,
        EvidenceChunk, EvidenceRoot, EvidenceRootBuilder, ExactAlleleSummaryTile,
        ExactDepthSummaryTile, ExactFilterSummaryTile, ExactJunctionSummaryTile,
        ExactModificationSummaryTile, ExactPairSummaryTile, FilteredRecordProvenance, GeometryLod,
        LayoutContract, PayloadContract, PayloadSidecar, PresentationContract, RevisionFingerprint,
        RowAtlasRevision, SceneRevision, SourceChunkId, SourceFingerprint, SummaryContract,
    };

    const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(77);

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn exact_authority(ledger: &AllocationLedger) -> (Arc<EvidenceRoot>, Arc<ExactSummaryRoot>) {
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 10,
            end: 23,
            flags: AlignmentFlags::from_bits(0x63),
            mapping_quality: Some(60),
            mate_contig: Some(ContigId::new(0)),
            mate_start: Some(50),
            template_length: 60,
            read_name: b"spliced-pair".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
                CigarOp {
                    kind: CigarOpKind::ReferenceSkip,
                    len: 3,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
            ],
            sequence: SequenceData::Present {
                bases: b"ACGTCACGTC".to_vec(),
                qualities: Some(vec![35; 10]),
            },
            auxiliary_fields: vec![
                AuxField {
                    tag: *b"XS",
                    value: AuxValue::Character(b'+'),
                },
                AuxField {
                    tag: *b"MM",
                    value: AuxValue::String("C+m,0;".to_owned()),
                },
                AuxField {
                    tag: *b"ML",
                    value: AuxValue::UnsignedArray(vec![220]),
                },
                AuxField {
                    tag: *b"HP",
                    value: AuxValue::Unsigned(1),
                },
            ],
        };
        let mut decoded = AlignmentBatch::default();
        decoded
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((5 << 16) | 1),
            )
            .unwrap();
        let payload_contract = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let payload =
            Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
        let core = crate::CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 5,
            },
            Arc::new(decoded.copy_core().unwrap()),
            RECORD_CONTRACT,
        )
        .unwrap();
        let payload_bytes = payload.estimated_bytes();
        let payload_allocation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(payload_bytes, 1)
            .unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let core_allocation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.core_bytes, 1)
            .unwrap();
        let overlap_allocation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let coverage = CompleteCoverage::new(
            source_revision(),
            RECORD_CONTRACT,
            interval,
            FilteredRecordProvenance {
                records_seen: 1,
                eligible_records: 1,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(6, 7)),
            source_revision(),
            RECORD_CONTRACT,
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        let evidence = builder.build(2).unwrap();
        let tile = CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(100).unwrap());
        let contract = SummaryContract {
            overlapping_mate_policy: OverlappingMatePolicy::CountOnce,
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            allele_detail: AlleleSummaryDetail::Counts,
            collect_modifications: true,
        };
        let depth = ExactDepthSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.depth(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let alleles = ExactAlleleSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.alleles(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let junctions = ExactJunctionSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.junctions(),
            Some(1_000),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let pairs = ExactPairSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.pairs(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let filters = ExactFilterSummaryTile::build(
            &evidence,
            tile,
            100,
            interval,
            contract.filters(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let modifications = ExactModificationSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.modifications().unwrap(),
            ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let root = ExactSummaryRoot::build(
            SummaryRevision::new(RevisionFingerprint::new(8, 9)),
            evidence.revision(),
            evidence.source_revision(),
            evidence.record_contract(),
            contract,
            interval,
            vec![depth],
            vec![alleles],
            vec![junctions],
            vec![pairs],
            vec![filters],
            vec![modifications],
            ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();
        (evidence, root)
    }

    fn exact_root(ledger: &AllocationLedger) -> Arc<ExactSummaryRoot> {
        exact_authority(ledger).1
    }

    #[test]
    fn every_derived_lane_is_a_raw_camera_independent_semantic_page() {
        let ledger = AllocationLedger::new(32 << 20, 0);
        let root = exact_root(&ledger);
        let tile = root.depth_tiles()[0].tile();
        let before = ledger.snapshot().unwrap().charged;
        let families = [
            SummaryPageFamily::Depth,
            SummaryPageFamily::Alleles,
            SummaryPageFamily::Junctions,
            SummaryPageFamily::Modifications,
            SummaryPageFamily::Pairs,
            SummaryPageFamily::FilterProvenance,
        ];
        let pages = families
            .into_iter()
            .enumerate()
            .map(|(index, family)| {
                SummarySemanticPage::build(
                    SummaryPageKey::new(&root, tile, family),
                    &root,
                    None,
                    &ledger,
                    ReservationKind::Normal,
                    10 + index as u64,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        assert!(pages.iter().all(|page| !page.primitives().is_empty()));
        assert!(pages[0].primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Depth { depth: 1, .. }
        )));
        assert!(pages[2].primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Junction(junction)
                if junction.evidence.start == 15 && junction.evidence.end == 18
        )));
        assert!(pages[3].primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Modification { .. }
        )));
        assert!(pages[4].primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::PairStatistics(_)
        )));
        let snapshot = ledger.snapshot().unwrap();
        assert!(snapshot.charged > before);
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::SummaryPageBuilder && usage.high_water > 0
        }));
    }

    #[test]
    fn stale_summary_key_cannot_publish_against_another_root() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let first = exact_root(&ledger);
        let second = exact_root(&ledger);
        let key = SummaryPageKey::new(
            &first,
            first.depth_tiles()[0].tile(),
            SummaryPageFamily::Depth,
        );
        // Both helpers intentionally use the same semantic revisions, proving compatible roots
        // can share a key; changing the revision makes staleness explicit.
        let stale = SummaryPageKey {
            summary_revision: SummaryRevision::new(RevisionFingerprint::new(99, 100)),
            ..key
        };
        assert_eq!(
            SummarySemanticPage::build(stale, &second, None, &ledger, ReservationKind::Normal, 20,)
                .unwrap_err(),
            SummarySemanticPageError::AuthorityMismatch
        );
    }

    #[test]
    fn read_and_derived_lanes_publish_as_one_pinned_scene_snapshot() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let (evidence, summaries) = exact_authority(&ledger);
        let interval = summaries.prepared_interval();
        let layout = LayoutContract {
            row_scale_class: 0,
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(20, 21)),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            20,
        )
        .unwrap();
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let tile = summaries.depth_tiles()[0].tile();
        let row_block = CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap());
        let read_page = AlignmentSemanticPage::build(
            AlignmentPageKey::new(
                evidence.source_revision(),
                evidence.revision(),
                atlas.revision(),
                layout.fingerprint(),
                presentation.page_fingerprint(crate::AlignmentPageFamily::ReadGeometry),
                tile,
                row_block,
                crate::AlignmentPageFamily::ReadGeometry,
                GeometryLod::Span,
            ),
            &evidence,
            &atlas,
            presentation,
            100,
            None,
            &ledger,
            ReservationKind::Normal,
            21,
        )
        .unwrap();
        let revision = SceneRevision::new(RevisionFingerprint::new(22, 23));
        let reads = AlignmentPresentationScene::new(
            revision,
            Arc::clone(&evidence),
            Arc::clone(&atlas),
            presentation,
            interval,
            0..1,
            vec![read_page],
            &ledger,
            ReservationKind::Normal,
            22,
        )
        .unwrap();
        let families = [
            SummaryPageFamily::Depth,
            SummaryPageFamily::Alleles,
            SummaryPageFamily::Junctions,
            SummaryPageFamily::Modifications,
            SummaryPageFamily::Pairs,
            SummaryPageFamily::FilterProvenance,
        ];
        let pages = families
            .into_iter()
            .enumerate()
            .map(|(index, family)| {
                SummarySemanticPage::build(
                    SummaryPageKey::new(&summaries, tile, family),
                    &summaries,
                    None,
                    &ledger,
                    ReservationKind::Normal,
                    30 + index as u64,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let missing = AlignmentSceneSnapshot::new(
            revision,
            Arc::clone(&reads),
            Arc::clone(&summaries),
            pages[..pages.len() - 1].to_vec(),
            &ledger,
            ReservationKind::Normal,
            40,
        )
        .unwrap_err();
        assert!(matches!(
            missing,
            AlignmentSceneSnapshotError::MissingMandatorySummaryPage(_)
        ));

        let snapshot = AlignmentSceneSnapshot::new(
            revision,
            reads,
            Arc::clone(&summaries),
            pages,
            &ledger,
            ReservationKind::Normal,
            41,
        )
        .unwrap();
        assert!(snapshot.covers(interval, 0..1));
        assert_eq!(snapshot.summary_pages().len(), 6);
        assert_eq!(snapshot.reads().revision(), snapshot.revision());
        assert_eq!(
            snapshot.reads().evidence().revision(),
            snapshot.summaries().evidence_revision()
        );
        assert!(snapshot.metadata_allocation().snapshot().unwrap().pins > 0);

        let summary_camera = crate::CpuSummaryCamera::new(
            interval,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 1_000.0,
                height: 80.0,
            },
        )
        .unwrap();
        ledger.set_transient_budget(0).unwrap();
        assert!(matches!(
            crate::CpuSummaryProjector::project(
                &snapshot,
                0,
                summary_camera,
                &crate::CpuSummaryProjectionOptions::Depth(Default::default()),
            )
            .unwrap_err(),
            crate::CpuSummaryProjectionError::Ledger(LedgerError::TransientBudgetExceeded {
                class: TransientResourceClass::CpuSummaryProjection,
                ..
            })
        ));
        assert_eq!(ledger.snapshot().unwrap().transient, 0);
        ledger.set_transient_budget(64 << 20).unwrap();
        let accounted_frame = crate::CpuSummaryProjector::project(
            &snapshot,
            0,
            summary_camera,
            &crate::CpuSummaryProjectionOptions::Depth(Default::default()),
        )
        .unwrap();
        assert!(
            ledger
                .snapshot()
                .unwrap()
                .transient_resources
                .iter()
                .any(|usage| {
                    usage.class == TransientResourceClass::CpuSummaryProjection && usage.current > 0
                })
        );
        let accounted_clone = accounted_frame.clone();
        assert!(Arc::ptr_eq(
            &accounted_frame.primitives,
            &accounted_clone.primitives
        ));
        drop(accounted_frame);
        assert!(ledger.snapshot().unwrap().transient > 0);
        drop(accounted_clone);
        assert_eq!(ledger.snapshot().unwrap().transient, 0);

        let projection_options = [
            crate::CpuSummaryProjectionOptions::Depth(Default::default()),
            crate::CpuSummaryProjectionOptions::Alleles(crate::CpuAlleleProjectionOptions {
                minimum_fraction: 0.0,
                quality_weighted: true,
            }),
            crate::CpuSummaryProjectionOptions::Junctions(crate::CpuJunctionProjectionOptions {
                display_mode: rgv_core::JunctionDisplayMode::Expanded,
                count_mode: rgv_core::SashimiCountMode::CountsPerMillion,
                ..Default::default()
            }),
            crate::CpuSummaryProjectionOptions::Modifications(Default::default()),
            crate::CpuSummaryProjectionOptions::Pairs,
            crate::CpuSummaryProjectionOptions::FilterProvenance,
        ];
        let charged_before_projection = ledger.snapshot().unwrap().charged;
        for (sequence, options) in projection_options.iter().enumerate() {
            let frame = crate::CpuSummaryProjector::project(
                &snapshot,
                sequence as u64,
                summary_camera,
                options,
            )
            .unwrap();
            assert_eq!(frame.family, options.family());
            assert_eq!(frame.covered_interval, Some(interval));
            assert!(!frame.primitives.is_empty());
        }
        assert_eq!(
            ledger.snapshot().unwrap().charged,
            charged_before_projection
        );

        let mut store = AlignmentPresentationStore::new(ledger.clone());
        let candidate = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        assert!(store.current().is_none());
        assert_eq!(store.read_page_count(), 1);
        assert_eq!(store.summary_page_count(), 6);
        let current = store.publish(candidate);
        assert!(store.current_covers(interval, 0..1));

        let resident = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        assert_eq!(store.read_page_count(), 1);
        assert_eq!(store.summary_page_count(), 6);
        assert!(Arc::ptr_eq(store.current().unwrap(), &current));
        let replacement = store.publish(resident);
        assert!(!Arc::ptr_eq(&replacement, &current));
        assert_eq!(
            replacement.reads().pages()[0].key(),
            current.reads().pages()[0].key()
        );

        let mut family_store = AlignmentPresentationStore::new(ledger.clone());
        let read_presentation = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            reference: None,
            annotation: None,
        };
        let first = family_store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                read_presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        let geometry = first
            .snapshot()
            .reads()
            .pages()
            .iter()
            .find(|page| page.key().family() == crate::AlignmentPageFamily::ReadGeometry)
            .cloned()
            .unwrap();
        assert_eq!(family_store.read_page_count(), 3);
        let labelled = PresentationContract {
            style: crate::AlignmentStyleContract {
                read_labels: crate::AlignmentReadLabelMode::ReadName,
                ..read_presentation.style
            },
            ..read_presentation
        };
        let second = family_store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                labelled,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        let reused_geometry = second
            .snapshot()
            .reads()
            .pages()
            .iter()
            .find(|page| page.key().family() == crate::AlignmentPageFamily::ReadGeometry)
            .unwrap();
        assert!(Arc::ptr_eq(&geometry, reused_geometry));
        assert_eq!(family_store.read_page_count(), 4);
    }

    #[test]
    fn presentation_pressure_evicts_inactive_pages_without_mutating_current() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let (evidence, summaries) = exact_authority(&ledger);
        let interval = summaries.prepared_interval();
        let layout = LayoutContract {
            row_scale_class: 0,
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(30, 31)),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            30,
        )
        .unwrap();
        let span = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        };
        let mut store = AlignmentPresentationStore::new(ledger.clone());
        let candidate = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                span,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        let current = store.publish(candidate);
        let current_pointer = Arc::as_ptr(&current);
        let baseline_read_pages = store.read_page_count();

        let detailed = PresentationContract {
            geometry_lod: GeometryLod::Read,
            style: crate::AlignmentStyleContract::default(),
            ..span
        };
        let inactive = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                detailed,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();
        assert!(store.read_page_count() > baseline_read_pages);
        drop(inactive);

        let pinned = ledger.snapshot().unwrap().pinned;
        let pressure = store.reduce_budget(pinned).unwrap();
        assert!(pressure.evicted_read_pages > 0);
        assert_eq!(pressure.remaining_overage, 0);
        assert_eq!(store.read_page_count(), baseline_read_pages);
        assert_eq!(Arc::as_ptr(store.current().unwrap()), current_pointer);
        assert!(store.current_covers(interval, 0..1));

        let pinned_pressure = store.reduce_budget(0).unwrap();
        assert_eq!(pinned_pressure.charged, pinned_pressure.pinned);
        assert_eq!(pinned_pressure.pinned_overage, pinned_pressure.pinned);
        assert_eq!(pinned_pressure.remaining_overage, pinned_pressure.charged);

        let distant = GenomicInterval::new(ContigId::new(0), 200, 300).unwrap();
        let discarded = store.discard_disjoint_cache(distant, 0..1);
        assert_eq!(discarded.read_pages, baseline_read_pages);
        assert_eq!(discarded.summary_pages, 6);
        assert_eq!(store.read_page_count(), 0);
        assert_eq!(store.summary_page_count(), 0);
        assert!(current.covers(interval, 0..1));
        assert_eq!(ledger.snapshot().unwrap().charged, pinned_pressure.charged);
    }

    #[test]
    fn junction_annotation_status_and_labels_publish_in_the_same_scene_epoch() {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let (evidence, summaries) = exact_authority(&ledger);
        let interval = summaries.prepared_interval();
        let layout = LayoutContract {
            row_scale_class: 0,
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
        };
        let atlas = AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(40, 41)),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            40,
        )
        .unwrap();
        let annotation_revision = crate::AnnotationRevision::new(RevisionFingerprint::new(50, 51));
        let annotation = crate::JunctionAnnotationIndex::build(
            annotation_revision,
            1,
            vec![interval],
            vec![crate::JunctionAnnotationInput {
                contig: interval.contig(),
                start: 15,
                end: 18,
                labels: vec!["GENE1 / TX1".to_owned()],
            }],
            &ledger,
            ReservationKind::Normal,
            50,
        )
        .unwrap();
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract {
                show_modified_bases: false,
                ..crate::AlignmentStyleContract::default()
            },
            reference: None,
            annotation: Some(annotation_revision),
        };
        let mut store = AlignmentPresentationStore::new(ledger.clone());
        let missing = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap_err();
        assert!(matches!(
            missing,
            crate::AlignmentPresentationStoreError::AnnotationAuthorityMismatch
        ));

        let candidate = store
            .prepare_with_annotation(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..1,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                None,
                Some(&annotation),
                ReservationKind::Normal,
            )
            .unwrap();
        let snapshot = store.publish(candidate);
        assert!(Arc::ptr_eq(snapshot.annotation().unwrap(), &annotation));
        assert_eq!(
            snapshot
                .annotation()
                .unwrap()
                .labels(interval.contig(), 15, 18)[0]
                .as_ref(),
            "GENE1 / TX1"
        );
        let junction_page = snapshot
            .summary_pages()
            .iter()
            .find(|page| page.key().family() == SummaryPageFamily::Junctions)
            .unwrap();
        assert_eq!(
            junction_page.key().annotation_revision(),
            Some(annotation_revision)
        );
        assert!(junction_page.primitives().iter().any(|primitive| matches!(
            primitive.component,
            SummarySemanticComponent::Junction(AnnotatedJunctionSemantic {
                annotation: Some(crate::JunctionAnnotationStatus::Annotated),
                ..
            })
        )));

        let camera = crate::CpuSummaryCamera::new(
            interval,
            crate::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: 80.0,
            },
        )
        .unwrap();
        let frame = crate::CpuSummaryProjector::project(
            &snapshot,
            1,
            camera,
            &crate::CpuSummaryProjectionOptions::Junctions(Default::default()),
        )
        .unwrap();
        assert!(frame.primitives.iter().any(|primitive| matches!(
            primitive.component,
            crate::CpuSummaryProjectedComponent::JunctionAnnotation {
                status: crate::JunctionAnnotationStatus::Annotated,
                ..
            }
        )));
    }
}
