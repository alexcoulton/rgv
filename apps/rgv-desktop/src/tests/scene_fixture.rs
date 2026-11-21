use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentPageFamily, AlignmentPageKey, AlignmentPresentationScene, AlignmentReadLabelMode,
    AlignmentRowAtlas, AlignmentSceneSnapshot, AlignmentSemanticPage, AlignmentSourceRevision,
    AlignmentStyleContract, AlleleSummaryDetail, AllocationClass, AllocationLedger,
    CanonicalGenomicTile, CanonicalRecordChunk, CanonicalRowBlock, CapabilityRequirement,
    CompleteCoverage, ContractFingerprint, EvidenceAcquisition, EvidenceChunk, EvidenceRevision,
    EvidenceRoot, EvidenceRootBuilder, ExactAlleleSummaryTile, ExactDepthSummaryTile,
    ExactFilterSummaryTile, ExactJunctionSummaryTile, ExactModificationSummaryTile,
    ExactPairSummaryTile, ExactSummaryRoot, FilteredRecordProvenance, GeometryLod, LayoutContract,
    PayloadContract, PayloadSidecar, PresentationContract, ReferenceRevision, ReservationKind,
    RevisionFingerprint, RowAtlasRevision, SceneRevision, SourceChunkId, SourceFingerprint,
    SummaryContract, SummaryPageFamily, SummaryPageKey, SummaryRevision, SummarySemanticPage,
};
use rgv_core::{
    AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId, GenomicInterval,
    OverlappingMatePolicy, ReadStackingMode,
};
use rgv_hts::{
    AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
    AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, ReferenceSlice,
    SequenceData, TemplateLengthBounds,
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
        read_name: b"scene-export-read".to_vec(),
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
                tag: *b"MM",
                value: AuxValue::String("C+m,0;".to_owned()),
            },
            AuxField {
                tag: *b"ML",
                value: AuxValue::UnsignedArray(vec![220]),
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
    let payload = Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
    let core = CanonicalRecordChunk::new(
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
        PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation).unwrap(),
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
    let summaries = ExactSummaryRoot::build(
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
    (evidence, summaries)
}

pub(super) fn alignment_scene_snapshot() -> Arc<AlignmentSceneSnapshot> {
    let ledger = AllocationLedger::new_with_transient_budget(8 << 20, 8 << 20, 8 << 20);
    let (evidence, summaries) = exact_authority(&ledger);
    let interval = summaries.prepared_interval();
    let reference_revision = ReferenceRevision::new(RevisionFingerprint::new(30, 31));
    let reference = ReferenceSlice {
        interval,
        bases: vec![b'A'; interval.len() as usize],
    };
    let layout = LayoutContract {
        stacking: ReadStackingMode::Compact,
        grouping: AlignmentGroupMode::None,
        group_order: AlignmentOrderDirection::Natural,
        sorting: AlignmentSortMode::Start,
        sort_order: AlignmentOrderDirection::Natural,
        sort_anchor: None,
        view_as_pairs: false,
        pair_length_bounds: TemplateLengthBounds::default(),
        promoted_template: None,
        sampling: rgv_alignment_scene::ReadSampling::Complete,
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
        style: AlignmentStyleContract {
            read_labels: AlignmentReadLabelMode::Hidden,
            show_modified_bases: false,
            ..AlignmentStyleContract::default()
        },
        reference: Some(reference_revision),
        annotation: None,
    };
    let tile = summaries.depth_tiles()[0].tile();
    let read_page = AlignmentSemanticPage::build(
        AlignmentPageKey::new(
            evidence.source_revision(),
            evidence.revision(),
            atlas.revision(),
            layout.fingerprint(),
            presentation.page_fingerprint(AlignmentPageFamily::ReadGeometry),
            tile,
            CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap()),
            AlignmentPageFamily::ReadGeometry,
            GeometryLod::Span,
        ),
        &evidence,
        &atlas,
        presentation,
        100,
        Some(&reference),
        &ledger,
        ReservationKind::Normal,
        21,
    )
    .unwrap();
    let revision = SceneRevision::new(RevisionFingerprint::new(22, 23));
    let reads = AlignmentPresentationScene::new(
        revision,
        Arc::clone(&evidence),
        atlas,
        presentation,
        interval,
        0..1,
        vec![read_page],
        &ledger,
        ReservationKind::Normal,
        22,
    )
    .unwrap();
    let pages = [
        SummaryPageFamily::Depth,
        SummaryPageFamily::Alleles,
        SummaryPageFamily::Junctions,
        SummaryPageFamily::Modifications,
        SummaryPageFamily::Pairs,
        SummaryPageFamily::FilterProvenance,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, family)| {
        SummarySemanticPage::build_with_dependencies(
            SummaryPageKey::new_with_dependencies(
                &summaries,
                tile,
                family,
                Some(reference_revision),
                None,
            ),
            &summaries,
            (family == SummaryPageFamily::Alleles).then_some((
                reference_revision,
                &reference,
                interval.end(),
            )),
            None,
            &ledger,
            ReservationKind::Normal,
            40 + index as u64,
        )
        .unwrap()
    })
    .collect();
    AlignmentSceneSnapshot::new(
        revision,
        reads,
        summaries,
        pages,
        &ledger,
        ReservationKind::Normal,
        50,
    )
    .unwrap()
}
