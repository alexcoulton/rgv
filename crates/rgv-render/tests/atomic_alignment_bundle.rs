use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentCoverageLaneOptions, AlignmentInteractionState, AlignmentPageFamily, AlignmentPageKey,
    AlignmentPresentationScene, AlignmentReadLabelMode, AlignmentRowAtlas, AlignmentSceneSnapshot,
    AlignmentSemanticPage, AlignmentSourceRevision, AlignmentStyleContract, AlleleSummaryDetail,
    AllocationBundleActual, AllocationBundleSlot, AllocationBundleSpec, AllocationClass,
    AllocationLedger, AllocationRetirementClaimSet, AllocationRetirementToken,
    CanonicalGenomicTile, CanonicalRecordChunk, CanonicalRowBlock, CapabilityRequirement,
    CompleteCoverage, ContractFingerprint, CoverageAlleleInterpretation,
    CpuAlleleProjectionOptions, CpuClipRect, CpuDepthProjectionOptions, CpuSummaryCamera,
    EvidenceAcquisition, EvidenceChunk, EvidenceRevision, EvidenceRoot, EvidenceRootBuilder,
    ExactAlleleSummaryTile, ExactDepthSummaryTile, ExactFilterSummaryTile,
    ExactJunctionSummaryTile, ExactPairSummaryTile, ExactSummaryRoot, FilteredRecordProvenance,
    GeometryLod, LayoutContract, PayloadContract, PayloadSidecar, PresentationContract,
    ReferenceRevision, ReservationKind, RevisionFingerprint, RowAtlasRevision, SceneRevision,
    SourceChunkId, SourceFingerprint, SummaryContract, SummaryPageFamily, SummaryPageKey,
    SummaryRevision, SummarySemanticPage,
};
use rgv_core::{
    AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId,
    CoverageScaleTransform, GenomicInterval, OverlappingMatePolicy, ReadStackingMode,
};
use rgv_hts::{
    AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
    AlignmentSourcePosition, CigarOp, CigarOpKind, ReferenceSlice, SequenceData,
    TemplateLengthBounds,
};
use rgv_render::{
    AlignmentGpuBatchAdmission, AlignmentGpuFrame, AlignmentGpuPrepareStats, AlignmentGpuRenderer,
    AlignmentGpuRetirement, AlignmentGpuSceneRequest, AlignmentGpuScissor,
    AlignmentSummaryFrameOverlays, AlignmentSummaryGpuBatchAdmission, AlignmentSummaryGpuFrame,
    AlignmentSummaryGpuPrepareStats, AlignmentSummaryGpuRenderer, AlignmentSummaryGpuRetirement,
    AlignmentSummaryGpuSceneRequest,
};

const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(77);
const CAMERA_EPOCH: u64 = 41;

struct CombinedRetirement {
    claims: AllocationRetirementClaimSet,
    _reads: AlignmentGpuRetirement,
    _summaries: AlignmentSummaryGpuRetirement,
}

impl AllocationRetirementToken for CombinedRetirement {
    fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claims
    }
}

struct PublishedPair {
    reads: AlignmentGpuFrame,
    summaries: AlignmentSummaryGpuFrame,
    read_stats: AlignmentGpuPrepareStats,
    summary_stats: AlignmentSummaryGpuPrepareStats,
    read_admission: AlignmentGpuBatchAdmission,
    summary_admission: AlignmentSummaryGpuBatchAdmission,
    retired_claims: usize,
}

#[test]
fn read_and_multi_lane_summary_replace_as_one_atomic_gpu_bundle() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let interval = scene.prepared_interval();
    let read_camera = rgv_alignment_scene::AlignmentCamera::new(
        interval,
        0.0,
        12.0,
        9.0,
        CpuClipRect {
            x: 0.0,
            y: 96.0,
            width: 256.0,
            height: 128.0,
        },
    )
    .unwrap();
    let summary_camera = CpuSummaryCamera::new(
        interval,
        CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 96.0,
        },
    )
    .unwrap();
    let interaction = AlignmentInteractionState::default();
    let read_requests = [AlignmentGpuSceneRequest {
        scene: scene.reads(),
        camera_sequence: CAMERA_EPOCH,
        camera: read_camera,
        screen_size_points: [256.0, 224.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 96,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    }];
    let summary_requests = [
        summary_request(&scene, summary_camera, false),
        summary_request(&scene, summary_camera, true),
    ];
    let mut read_renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut summary_renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);

    let first = replace_pair(
        &device,
        &queue,
        &ledger,
        &mut read_renderer,
        &mut summary_renderer,
        &read_requests,
        &summary_requests,
        None,
        Vec::new(),
        100,
    );
    assert_eq!(first.retired_claims, 0);
    assert!(first.read_stats.geometry_uploaded_bytes > 0);
    assert!(first.summary_stats.geometry_uploaded_bytes > 0);
    assert!(first.read_admission.admitted_pages > 0);
    assert!(first.summary_admission.admitted_pages > 0);
    assert_eq!(first.reads.camera_sequences.as_ref(), [CAMERA_EPOCH]);
    assert_eq!(
        first.summaries.camera_sequences.as_ref(),
        [CAMERA_EPOCH, CAMERA_EPOCH]
    );
    assert_eq!(first.summaries.scene_revisions.len(), 2);

    // Replace both renderers again from the identical camera and semantic authority. Only the
    // camera/frame presentation objects change; immutable page geometry must stay resident.
    let second = replace_pair(
        &device,
        &queue,
        &ledger,
        &mut read_renderer,
        &mut summary_renderer,
        &read_requests,
        &summary_requests,
        Some(first.reads),
        vec![first.summaries],
        101,
    );
    assert!(
        second.retired_claims > 0,
        "the replacement must settle a real combined old-presentation claim"
    );
    assert_eq!(second.read_stats.geometry_uploaded_bytes, 0);
    assert_eq!(second.summary_stats.geometry_uploaded_bytes, 0);
    assert_eq!(second.read_admission.admitted_pages, 0);
    assert_eq!(second.summary_admission.admitted_pages, 0);
    assert_eq!(second.reads.camera_sequences.as_ref(), [CAMERA_EPOCH]);
    assert_eq!(
        second.summaries.camera_sequences.as_ref(),
        [CAMERA_EPOCH, CAMERA_EPOCH]
    );

    let steady = ledger.snapshot().unwrap();
    assert_eq!(steady.active_replacement_bundle, None);
    assert_eq!(steady.reserved_transition, 0);
    assert!(steady.charged <= steady.budget);
}

#[allow(clippy::too_many_arguments)]
fn replace_pair(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    ledger: &AllocationLedger,
    read_renderer: &mut AlignmentGpuRenderer,
    summary_renderer: &mut AlignmentSummaryGpuRenderer,
    read_requests: &[AlignmentGpuSceneRequest<'_>],
    summary_requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    mut prior_read: Option<AlignmentGpuFrame>,
    mut prior_summaries: Vec<AlignmentSummaryGpuFrame>,
    publication_revision: u64,
) -> PublishedPair {
    let snapshot = ledger.snapshot().unwrap();
    let first_slot = AllocationBundleSlot::new(0);
    let read_plan = read_renderer
        .plan_replacement(read_requests, &snapshot, first_slot)
        .unwrap();
    let summary_plan = summary_renderer
        .plan_replacement(summary_requests, &snapshot, read_plan.next_bundle_slot())
        .unwrap();
    let bundle_slot_end = summary_plan.next_bundle_slot();

    assert_eq!(read_plan.first_bundle_slot(), first_slot);
    assert_eq!(
        summary_plan.first_bundle_slot(),
        read_plan.next_bundle_slot()
    );
    assert_eq!(read_plan.policy_revision(), summary_plan.policy_revision());
    let mut demands = read_plan.demands().to_vec();
    demands.extend_from_slice(summary_plan.demands());
    let slots = demands
        .iter()
        .map(|demand| demand.slot.get())
        .collect::<Vec<_>>();
    assert_eq!(
        slots,
        (first_slot.get()..bundle_slot_end.get()).collect::<Vec<_>>(),
        "the two renderer plans must occupy one disjoint contiguous slot range"
    );

    let read_retirement = read_renderer
        .detach_replacement_retirement(ledger, &read_plan, &mut prior_read)
        .unwrap();
    let summary_retirement = summary_renderer
        .detach_replacement_retirement(ledger, &summary_plan, &mut prior_summaries)
        .unwrap();
    assert!(prior_read.is_none());
    assert!(prior_summaries.is_empty());
    let claims = ledger
        .combine_retirement_claim_sets([
            read_retirement.retirement_claim_set(),
            summary_retirement.retirement_claim_set(),
        ])
        .unwrap();
    let retired_claims = claims.len();
    let retirement = CombinedRetirement {
        claims: claims.clone(),
        _reads: read_retirement,
        _summaries: summary_retirement,
    };
    let bundle = ledger
        .reserve_replacement_bundle(AllocationBundleSpec::new(
            snapshot.policy_revision,
            demands,
            claims,
        ))
        .unwrap();
    let reserved = ledger.snapshot().unwrap();
    assert!(reserved.active_replacement_bundle.is_some());
    assert_eq!(reserved.reserved_transition, bundle.maximum_bytes());

    let staged_reads = read_renderer
        .stage_replacement(
            device,
            queue,
            read_requests,
            read_plan,
            &bundle,
            ledger,
            publication_revision,
        )
        .unwrap();
    let staged_summaries = summary_renderer
        .stage_replacement(
            device,
            queue,
            summary_requests,
            summary_plan,
            &bundle,
            ledger,
            publication_revision,
        )
        .unwrap();
    let ready_reads = read_renderer
        .preflight_replacement_publication(staged_reads)
        .unwrap();
    let ready_summaries = summary_renderer
        .preflight_replacement_publication(staged_summaries)
        .unwrap();
    let mut actuals = ready_reads.actuals().to_vec();
    actuals.extend_from_slice(ready_summaries.actuals());
    assert_actual_slots_match(&actuals, first_slot, bundle_slot_end);

    let commit = bundle.commit(actuals).unwrap();
    let mut retirement = Some(retirement);
    let mut settled_leases = commit.retire_and_settle(&mut retirement).unwrap();
    assert!(retirement.is_none());

    // These APIs intentionally have no Result after settlement: all recoverable validation ran
    // while the combined retirement token was still restorable.
    let publishable_reads = read_renderer.bind_replacement_leases(ready_reads, &mut settled_leases);
    let publishable_summaries =
        summary_renderer.bind_replacement_leases(ready_summaries, &mut settled_leases);
    assert!(settled_leases.is_empty());
    let (reads, read_stats, read_admission) = read_renderer.publish_replacement(publishable_reads);
    let (summaries, summary_stats, summary_admission) =
        summary_renderer.publish_replacement(publishable_summaries);

    let steady = ledger.snapshot().unwrap();
    assert_eq!(steady.active_replacement_bundle, None);
    assert_eq!(steady.reserved_transition, 0);
    assert!(steady.charged <= steady.budget);
    PublishedPair {
        reads,
        summaries,
        read_stats,
        summary_stats,
        read_admission,
        summary_admission,
        retired_claims,
    }
}

fn assert_actual_slots_match(
    actuals: &[AllocationBundleActual],
    first: AllocationBundleSlot,
    end: AllocationBundleSlot,
) {
    let mut slots = actuals
        .iter()
        .map(|actual| actual.slot.get())
        .collect::<Vec<_>>();
    slots.sort_unstable();
    assert_eq!(slots, (first.get()..end.get()).collect::<Vec<_>>());
}

fn summary_request<'a>(
    scene: &'a Arc<AlignmentSceneSnapshot>,
    camera: CpuSummaryCamera,
    alleles: bool,
) -> AlignmentSummaryGpuSceneRequest<'a> {
    AlignmentSummaryGpuSceneRequest {
        scene,
        camera_sequence: CAMERA_EPOCH,
        camera,
        screen_size_points: [256.0, 224.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 96,
        },
        options: AlignmentCoverageLaneOptions {
            depth: CpuDepthProjectionOptions {
                scale_maximum: None,
                transform: CoverageScaleTransform::Linear,
                show_center_line: false,
                selected_position: None,
            },
            alleles: alleles.then_some(CpuAlleleProjectionOptions {
                minimum_fraction: 0.0,
                quality_weighted: false,
                scale_maximum: None,
                transform: CoverageScaleTransform::Linear,
                interpretation: CoverageAlleleInterpretation::GenericReferenceAware,
            }),
        }
        .into(),
        overlays: AlignmentSummaryFrameOverlays::default(),
    }
}

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
        read_name: b"atomic-bundle".to_vec(),
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
        auxiliary_fields: Vec::new(),
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
        collect_modifications: false,
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
        Vec::new(),
        ledger,
        ReservationKind::Normal,
        4,
    )
    .unwrap();
    (evidence, summaries)
}

fn test_snapshot(ledger: &AllocationLedger) -> Arc<AlignmentSceneSnapshot> {
    let (evidence, summaries) = exact_authority(ledger);
    let interval = summaries.prepared_interval();
    let reference_revision = ReferenceRevision::new(RevisionFingerprint::new(30, 31));
    let reference = ReferenceSlice {
        interval,
        bases: vec![b'A'; interval.len() as usize],
    };
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
        ledger,
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
        ledger,
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
        ledger,
        ReservationKind::Normal,
        22,
    )
    .unwrap();
    let pages = [
        SummaryPageFamily::Depth,
        SummaryPageFamily::Alleles,
        SummaryPageFamily::Junctions,
        SummaryPageFamily::Pairs,
        SummaryPageFamily::FilterProvenance,
    ]
    .into_iter()
    .enumerate()
    .map(|(index, family)| {
        let key = SummaryPageKey::new_with_dependencies(
            &summaries,
            tile,
            family,
            Some(reference_revision),
            None,
        );
        SummarySemanticPage::build_with_dependencies(
            key,
            &summaries,
            (family == SummaryPageFamily::Alleles).then_some((
                reference_revision,
                &reference,
                interval.end(),
            )),
            None,
            ledger,
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
        ledger,
        ReservationKind::Normal,
        50,
    )
    .unwrap()
}

fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("RGV atomic alignment bundle integration test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .ok()
}
