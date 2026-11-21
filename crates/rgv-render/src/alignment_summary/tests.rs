use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentCoherenceGroupId, AlignmentPageFamily, AlignmentPageKey, AlignmentPresentationScene,
    AlignmentReadLabelMode, AlignmentRowAtlas, AlignmentSceneSnapshot, AlignmentSemanticPage,
    AlignmentSourceRevision, AlignmentStyleContract, AlleleSummaryDetail, AllocationBundleSlot,
    AllocationBundleSpec, AllocationClass, AllocationLedger, CanonicalGenomicTile,
    CanonicalRecordChunk, CanonicalRowBlock, CapabilityRequirement, CompleteCoverage,
    ContractFingerprint, CoverageAlleleInterpretation, CpuAlleleProjectionOptions,
    CpuDepthProjectionOptions, CpuJunctionProjectionOptions, CpuModificationProjectionOptions,
    CpuSummaryProjectedComponent, CpuSummaryProjectionOptions, CpuSummaryProjector,
    EvidenceAcquisition, EvidenceChunk, EvidenceRevision, EvidenceRoot, EvidenceRootBuilder,
    ExactAlleleSummaryTile, ExactDepthSummaryTile, ExactFilterSummaryTile,
    ExactJunctionSummaryTile, ExactModificationSummaryTile, ExactPairSummaryTile, ExactSummaryRoot,
    FilteredRecordProvenance, GeometryLod, LayoutContract, PayloadContract, PayloadSidecar,
    PresentationContract, ReferenceRevision, ReservationKind, RevisionFingerprint,
    RowAtlasRevision, SceneRevision, SourceChunkId, SourceFingerprint, SummaryContract,
    SummaryRevision,
};
use rgv_core::{
    AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, BisulfiteContext, ContigId,
    GenomicInterval, JunctionDisplayMode, OverlappingMatePolicy, ReadStackingMode,
    SashimiCountMode, Viewport,
};
use rgv_model::{
    AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
    AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, ReferenceSlice,
    SequenceData, TemplateLengthBounds,
};

use super::*;

const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(77);

fn source_revision() -> AlignmentSourceRevision {
    AlignmentSourceRevision::new(
        SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
        RevisionFingerprint::new(3, 4),
    )
}

/// Evidence always covers `0..100`; the summary root is prepared over `summary_start..100`,
/// so a junction starting left of `summary_start` enters its first tile.
fn exact_authority_over(
    ledger: &AllocationLedger,
    tile_bases: NonZeroU64,
    summary_start: u64,
) -> (Arc<EvidenceRoot>, Arc<ExactSummaryRoot>) {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
    let summary_interval = GenomicInterval::new(ContigId::new(0), summary_start, 100).unwrap();
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 10,
        end: 23,
        flags: AlignmentFlags::from_bits(0x63),
        mapping_quality: Some(60),
        mate_contig: Some(ContigId::new(0)),
        mate_start: Some(50),
        template_length: 60,
        read_name: b"summary-gpu".to_vec(),
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
                value: AuxValue::String("C+m,0;C+h,0;C+a,0;".to_owned()),
            },
            AuxField {
                tag: *b"ML",
                value: AuxValue::UnsignedArray(vec![255, 128, 64]),
            },
            AuxField {
                tag: *b"HP",
                value: AuxValue::Unsigned(2),
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
    let contract = SummaryContract {
        overlapping_mate_policy: OverlappingMatePolicy::CountOnce,
        minimum_base_quality: 0,
        minimum_junction_flank: 0,
        allele_detail: AlleleSummaryDetail::Counts,
        collect_modifications: true,
    };
    let mut depth = Vec::new();
    let mut alleles = Vec::new();
    let mut junctions = Vec::new();
    let mut pairs = Vec::new();
    let mut filters = Vec::new();
    let mut modifications = Vec::new();
    for (index, tile) in CanonicalGenomicTile::covering(summary_interval, tile_bases, 100)
        .unwrap()
        .into_iter()
        .enumerate()
    {
        let revision = 3_u64.saturating_add(index as u64);
        depth.push(
            ExactDepthSummaryTile::build(
                &evidence,
                tile,
                100,
                contract.depth(),
                ledger,
                ReservationKind::Normal,
                revision,
            )
            .unwrap(),
        );
        alleles.push(
            ExactAlleleSummaryTile::build(
                &evidence,
                tile,
                100,
                contract.alleles(),
                ledger,
                ReservationKind::Normal,
                revision,
            )
            .unwrap(),
        );
        junctions.push(
            ExactJunctionSummaryTile::build(
                &evidence,
                tile,
                100,
                contract.junctions(),
                Some(1_000),
                ledger,
                ReservationKind::Normal,
                revision,
            )
            .unwrap(),
        );
        pairs.push(
            ExactPairSummaryTile::build(
                &evidence,
                tile,
                100,
                contract.pairs(),
                ledger,
                ReservationKind::Normal,
                revision,
            )
            .unwrap(),
        );
        filters.push(
            ExactFilterSummaryTile::build(
                &evidence,
                tile,
                100,
                summary_interval,
                contract.filters(),
                ledger,
                ReservationKind::Normal,
                revision,
            )
            .unwrap(),
        );
        modifications.push(
            ExactModificationSummaryTile::build(
                &evidence,
                tile,
                100,
                contract.modifications().unwrap(),
                ledger,
                ReservationKind::Normal,
                revision,
            )
            .unwrap(),
        );
    }
    let root = ExactSummaryRoot::build(
        SummaryRevision::new(RevisionFingerprint::new(8, 9)),
        evidence.revision(),
        evidence.source_revision(),
        evidence.record_contract(),
        contract,
        summary_interval,
        depth,
        alleles,
        junctions,
        pairs,
        filters,
        modifications,
        ledger,
        ReservationKind::Normal,
        4,
    )
    .unwrap();
    (evidence, root)
}

pub(crate) fn test_snapshot(ledger: &AllocationLedger) -> Arc<AlignmentSceneSnapshot> {
    test_snapshot_with_prepared_interval(ledger, None)
}

fn test_snapshot_with_prepared_interval(
    ledger: &AllocationLedger,
    prepared_interval: Option<GenomicInterval>,
) -> Arc<AlignmentSceneSnapshot> {
    test_snapshot_with_authority(
        ledger,
        prepared_interval,
        NonZeroU64::new(100).unwrap(),
        None,
    )
}

fn test_snapshot_with_summary_tile_bases(
    ledger: &AllocationLedger,
    tile_bases: NonZeroU64,
) -> Arc<AlignmentSceneSnapshot> {
    test_snapshot_with_authority(ledger, None, tile_bases, None)
}

pub(crate) fn test_snapshot_with_reference(
    ledger: &AllocationLedger,
    reference_bases: Vec<u8>,
) -> Arc<AlignmentSceneSnapshot> {
    test_snapshot_with_authority(
        ledger,
        None,
        NonZeroU64::new(100).unwrap(),
        Some(reference_bases),
    )
}

fn test_snapshot_with_authority(
    ledger: &AllocationLedger,
    prepared_interval: Option<GenomicInterval>,
    tile_bases: NonZeroU64,
    reference_bases: Option<Vec<u8>>,
) -> Arc<AlignmentSceneSnapshot> {
    test_snapshot_over(ledger, prepared_interval, tile_bases, 0, reference_bases)
}

fn test_snapshot_over(
    ledger: &AllocationLedger,
    prepared_interval: Option<GenomicInterval>,
    tile_bases: NonZeroU64,
    summary_start: u64,
    reference_bases: Option<Vec<u8>>,
) -> Arc<AlignmentSceneSnapshot> {
    let (evidence, summaries) = exact_authority_over(ledger, tile_bases, summary_start);
    let interval = summaries.prepared_interval();
    let prepared_interval = prepared_interval.unwrap_or(interval);
    let reference_revision = ReferenceRevision::new(RevisionFingerprint::new(30, 31));
    // The reference always spans the evidence, which starts left of a late summary root.
    let reference_interval = GenomicInterval::new(interval.contig(), 0, interval.end()).unwrap();
    let reference = ReferenceSlice {
        interval: reference_interval,
        bases: reference_bases.unwrap_or_else(|| vec![b'A'; reference_interval.len() as usize]),
    };
    assert_eq!(reference.bases.len(), reference_interval.len() as usize);
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
    let row_block = CanonicalRowBlock::new(0, NonZeroU32::new(16).unwrap());
    let mut read_pages = Vec::with_capacity(summaries.depth_tiles().len());
    for (index, tile) in summaries
        .depth_tiles()
        .iter()
        .map(|depth| depth.tile())
        .enumerate()
    {
        read_pages.push(
            AlignmentSemanticPage::build(
                AlignmentPageKey::new(
                    evidence.source_revision(),
                    evidence.revision(),
                    atlas.revision(),
                    layout.fingerprint(),
                    presentation.page_fingerprint(AlignmentPageFamily::ReadGeometry),
                    tile,
                    row_block,
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
                21 + index as u64,
            )
            .unwrap(),
        );
    }
    let revision = SceneRevision::new(RevisionFingerprint::new(22, 23));
    let reads = AlignmentPresentationScene::new(
        revision,
        Arc::clone(&evidence),
        atlas,
        presentation,
        prepared_interval,
        0..1,
        read_pages,
        ledger,
        ReservationKind::Normal,
        22,
    )
    .unwrap();
    let mut pages = Vec::new();
    for tile in summaries.depth_tiles().iter().map(|depth| depth.tile()) {
        for family in [
            SummaryPageFamily::Depth,
            SummaryPageFamily::Alleles,
            SummaryPageFamily::Junctions,
            SummaryPageFamily::Modifications,
            SummaryPageFamily::Pairs,
            SummaryPageFamily::FilterProvenance,
        ] {
            let key = SummaryPageKey::new_with_dependencies(
                &summaries,
                tile,
                family,
                Some(reference_revision),
                None,
            );
            let page = SummarySemanticPage::build_with_dependencies(
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
                40 + pages.len() as u64,
            )
            .unwrap();
            pages.push(page);
        }
    }
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
        label: Some("RGV alignment summary test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .ok()
}

fn camera(interval: GenomicInterval) -> CpuSummaryCamera {
    CpuSummaryCamera::new(
        interval,
        rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 96.0,
        },
    )
    .unwrap()
}

pub(crate) fn lane_options(alleles: bool) -> AlignmentCoverageLaneOptions {
    AlignmentCoverageLaneOptions {
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
}

fn request<'a>(
    scene: &'a Arc<AlignmentSceneSnapshot>,
    sequence: u64,
    camera: CpuSummaryCamera,
    options: AlignmentCoverageLaneOptions,
) -> AlignmentSummaryGpuSceneRequest<'a> {
    AlignmentSummaryGpuSceneRequest {
        scene,
        coherence_scope: 1,
        lane_identity: 1,
        camera_sequence: sequence,
        camera,
        screen_size_points: [256.0, 96.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 96,
        },
        options: options.into(),
        overlays: AlignmentSummaryFrameOverlays::default(),
    }
}

pub(crate) fn modification_options(
    probability_weighted: bool,
    hidden_codes: Vec<ModificationCode>,
    color_mode: ModifiedBaseColorMode,
) -> AlignmentModificationLaneOptions {
    AlignmentModificationLaneOptions {
        projection: CpuModificationProjectionOptions {
            probability_weighted,
            hidden_codes,
        },
        color_mode,
        selected_position: None,
    }
}

fn modification_request<'a>(
    scene: &'a Arc<AlignmentSceneSnapshot>,
    sequence: u64,
    camera: CpuSummaryCamera,
    options: AlignmentModificationLaneOptions,
) -> AlignmentSummaryGpuSceneRequest<'a> {
    AlignmentSummaryGpuSceneRequest {
        scene,
        coherence_scope: 1,
        lane_identity: 1,
        camera_sequence: sequence,
        camera,
        screen_size_points: [256.0, 96.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 96,
        },
        options: options.into(),
        overlays: AlignmentSummaryFrameOverlays::default(),
    }
}

pub(crate) fn sashimi_options(
    display_mode: JunctionDisplayMode,
    scale: AlignmentSashimiScalePolicy,
) -> AlignmentSashimiLaneOptions {
    AlignmentSashimiLaneOptions {
        projection: CpuJunctionProjectionOptions {
            minimum_depth: 1,
            minimum_flank: 0,
            display_mode,
            scale_maximum: None,
            count_mode: SashimiCountMode::Raw,
            show_labels: true,
        },
        scale,
    }
}

fn sashimi_request<'a>(
    scene: &'a Arc<AlignmentSceneSnapshot>,
    sequence: u64,
    camera: CpuSummaryCamera,
    coherence_scope: u64,
    lane_identity: u64,
    options: AlignmentSashimiLaneOptions,
) -> AlignmentSummaryGpuSceneRequest<'a> {
    AlignmentSummaryGpuSceneRequest {
        scene,
        coherence_scope,
        lane_identity,
        camera_sequence: sequence,
        camera,
        screen_size_points: [256.0, 96.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 96,
        },
        options: options.into(),
        overlays: AlignmentSummaryFrameOverlays::default(),
    }
}

fn publish_bundled_summary_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut AlignmentSummaryGpuRenderer,
    requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    ledger: &AllocationLedger,
    mut prior_frames: Vec<AlignmentSummaryGpuFrame>,
    first_slot: u32,
    publication_revision: u64,
) -> (
    AlignmentSummaryGpuFrame,
    AlignmentSummaryGpuPrepareStats,
    AlignmentSummaryGpuBatchAdmission,
) {
    let snapshot = ledger.snapshot().unwrap();
    let plan = renderer
        .plan_replacement(requests, &snapshot, AllocationBundleSlot::new(first_slot))
        .unwrap();
    let retirement = renderer
        .detach_replacement_retirement(ledger, &plan, &mut prior_frames)
        .unwrap();
    assert!(prior_frames.is_empty());
    let spec = AllocationBundleSpec::new(
        plan.policy_revision(),
        plan.demands().to_vec(),
        retirement.retirement_claim_set().clone(),
    );
    let bundle = ledger.reserve_replacement_bundle(spec).unwrap();
    let staged = renderer
        .stage_replacement(
            device,
            queue,
            requests,
            plan,
            &bundle,
            ledger,
            publication_revision,
        )
        .unwrap();
    let ready = renderer.preflight_replacement_publication(staged).unwrap();
    let actuals = ready.actuals().to_vec();
    let committed = bundle.commit(actuals).unwrap();
    let mut retirement = Some(retirement);
    let mut leases = committed.retire_and_settle(&mut retirement).unwrap();
    assert!(retirement.is_none());
    let publishable = renderer.bind_replacement_leases(ready, &mut leases);
    let published = renderer.publish_replacement(publishable);
    assert!(leases.is_empty());
    published
}

fn assert_near(left: f64, right: f64) {
    assert!((left - right).abs() < 1.0e-4, "{left} != {right}");
}

/// REND-8, summary side. Depth runs are twelve bytes (they were thirty-two) and refuse spans
/// they cannot hold; the other summary instance types are unchanged and pinned here so a later
/// change is deliberate.
#[test]
fn summary_instance_sizes_are_pinned_and_depth_spans_are_validated() {
    assert_eq!(size_of::<AlignmentSummaryGpuDepthInstance>(), 12);
    assert_eq!(size_of::<AlignmentSummaryGpuAlleleInstance>(), 64);
    assert_eq!(size_of::<AlignmentSummaryGpuModificationInstance>(), 72);
    assert_eq!(size_of::<AlignmentSummaryGpuJunctionInstance>(), 48);
    assert_eq!(size_of::<AlignmentSummaryGpuOverlayInstance>(), 24);
    assert_eq!(
        size_of::<AlignmentSummaryGpuDepthInstance>() % wgpu::COPY_BUFFER_ALIGNMENT as usize,
        0
    );

    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let key = scene
        .summary_pages()
        .iter()
        .find(|page| page.key().family() == SummaryPageFamily::Depth)
        .unwrap()
        .key();
    let limit = crate::MAXIMUM_ALIGNMENT_GPU_PAGE_OFFSET;
    let run = AlignmentSummaryGpuDepthInstance::new(key, limit - 3, limit, 9).unwrap();
    assert_eq!(
        (run.start_offset(), run.end_offset(), run.depth),
        (limit - 3, limit, 9)
    );
    assert!(matches!(
        AlignmentSummaryGpuDepthInstance::new(key, 4, limit + 1, 1),
        Err(AlignmentSummaryGpuPageError::SemanticOffsetOutOfRange { start: 4, .. })
    ));
    assert!(matches!(
        AlignmentSummaryGpuDepthInstance::new(key, u64::from(u32::MAX) + 2, u64::MAX, 1),
        Err(AlignmentSummaryGpuPageError::SemanticOffsetOutOfRange { .. })
    ));
    assert!(matches!(
        AlignmentSummaryGpuDepthInstance::new(key, 5, 5, 1),
        Err(AlignmentSummaryGpuPageError::InvalidSemanticSpan { .. })
    ));
}

#[test]
fn packed_depth_and_reference_aware_alleles_match_the_cpu_oracle() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let camera = camera(scene.prepared_interval());
    let depth_page = scene
        .summary_pages()
        .iter()
        .find(|page| page.key().family() == SummaryPageFamily::Depth)
        .unwrap();
    let allele_page = scene
        .summary_pages()
        .iter()
        .find(|page| page.key().family() == SummaryPageFamily::Alleles)
        .unwrap();

    let depth_options = CpuDepthProjectionOptions {
        scale_maximum: Some(1),
        transform: CoverageScaleTransform::Linear,
        show_center_line: false,
        selected_position: None,
    };
    let cpu_depth = CpuSummaryProjector::project(
        &scene,
        1,
        camera,
        &CpuSummaryProjectionOptions::Depth(depth_options),
    )
    .unwrap();
    let PackedAlignmentSummaryPage::Depth {
        instances,
        range_maximum,
    } = PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::depth(depth_page))
        .unwrap()
    else {
        unreachable!();
    };
    let mut gpu_depth = instances
        .iter()
        .filter_map(|instance| {
            project_alignment_summary_depth_instance(
                *instance,
                depth_page.interval().start(),
                camera,
                1,
                CoverageScaleTransform::Linear,
            )
        })
        .collect::<Vec<_>>();
    let mut cpu_depth = cpu_depth
        .primitives
        .iter()
        .filter(|primitive| {
            matches!(
                primitive.component,
                CpuSummaryProjectedComponent::Depth { .. }
            )
        })
        .collect::<Vec<_>>();
    gpu_depth.sort_by(|left, right| left.x.total_cmp(&right.x));
    cpu_depth.sort_by(|left, right| left.x.total_cmp(&right.x));
    assert_eq!(gpu_depth.len(), cpu_depth.len());
    for (gpu, cpu) in gpu_depth.iter().zip(cpu_depth) {
        assert_near(gpu.x, cpu.x);
        assert_near(gpu.y, cpu.y);
        assert_near(gpu.width, cpu.width);
        assert_near(gpu.height, cpu.height);
    }
    assert_eq!(range_maximum.maximum_depth_exact(9.5, 10.0), 0);
    assert_eq!(range_maximum.maximum_depth_exact(9.5, 10.5), 1);
    assert_eq!(range_maximum.maximum_depth_exact(23.0, 23.5), 0);

    let allele_options = CpuAlleleProjectionOptions {
        minimum_fraction: 0.0,
        quality_weighted: false,
        scale_maximum: Some(1),
        transform: CoverageScaleTransform::Linear,
        interpretation: CoverageAlleleInterpretation::GenericReferenceAware,
    };
    let cpu_alleles = CpuSummaryProjector::project(
        &scene,
        2,
        camera,
        &CpuSummaryProjectionOptions::Alleles(allele_options),
    )
    .unwrap();
    let PackedAlignmentSummaryPage::Alleles { instances } =
        PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::alleles(
            allele_page,
            depth_page,
        ))
        .unwrap()
    else {
        unreachable!();
    };
    let mut gpu_alleles = instances
        .iter()
        .filter_map(|instance| {
            project_alignment_summary_allele_instance(
                *instance,
                allele_page.interval().start(),
                camera,
                1,
                CoverageScaleTransform::Linear,
                0.0,
                false,
            )
        })
        .collect::<Vec<_>>();
    let mut cpu_alleles = cpu_alleles
        .primitives
        .iter()
        .filter(|primitive| {
            matches!(
                primitive.component,
                CpuSummaryProjectedComponent::Allele { .. }
            )
        })
        .collect::<Vec<_>>();
    gpu_alleles.sort_by(|left, right| {
        left.bounds
            .x
            .total_cmp(&right.bounds.x)
            .then_with(|| left.bounds.y.total_cmp(&right.bounds.y))
    });
    cpu_alleles.sort_by(|left, right| {
        left.x
            .total_cmp(&right.x)
            .then_with(|| left.y.total_cmp(&right.y))
    });
    assert_eq!(gpu_alleles.len(), cpu_alleles.len());
    for (gpu, cpu) in gpu_alleles.iter().zip(cpu_alleles) {
        assert_near(gpu.bounds.x, cpu.x);
        assert_near(gpu.bounds.y, cpu.y);
        assert_near(gpu.bounds.width, cpu.width);
        assert_near(gpu.bounds.height, cpu.height);
    }
}

#[test]
fn semantic_export_contains_committed_read_and_depth_geometry_and_pins_its_capture() {
    use crate::palette::{AlignmentPalette, PaletteToken};
    use rgv_alignment_scene::{
        AlignmentCamera, AlignmentDerivedLaneCapture, AlignmentDerivedLaneOptions,
        AlignmentPanelLaneFrame, AlignmentPanelSceneFrame, CpuClipRect, CpuPrimitiveShape,
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let interval = scene.prepared_interval();
    let viewport = Viewport::new(interval, 256.0).unwrap();
    let read_camera = AlignmentCamera::from_viewport(
        viewport,
        0.0,
        10.0,
        8.0,
        CpuClipRect {
            x: 0.0,
            y: 100.0,
            width: 256.0,
            height: f64::from(scene.reads().prepared_rows().end) * 10.0,
        },
    )
    .unwrap();
    let lane = AlignmentPanelLaneFrame {
        track_id: rgv_core::TrackId::new(1),
        preparation_camera: read_camera.into(),
        read_camera: Some(read_camera),
        row_activity: None,
        derived_lanes: vec![
            AlignmentDerivedLaneCapture::new(
                camera(interval).clip(),
                AlignmentDerivedLaneOptions::Coverage(lane_options(false)),
            )
            .unwrap(),
        ]
        .into(),
        source_camera_sequence: 1,
        snapshot: Some(Arc::clone(&scene)),
        covered_interval: Some(interval),
        // The fixture has one atlas row. Empty camera padding is not
        // evidence demand, matching panel-scene capture's atlas clipping.
        requested_rows: scene.reads().prepared_rows(),
        covered_rows: scene.reads().prepared_rows(),
        scene_contracts_current: true,
    };
    let frame = AlignmentPanelSceneFrame {
        panel_id: rgv_core::PanelId::new(1),
        viewport,
        camera_revision: 1,
        authority_revision: 1,
        lanes: vec![lane].into(),
        coherence_groups: Vec::new().into(),
    };
    let expected =
        rgv_alignment_scene::CpuAlignmentProjector::project(scene.reads(), 1, read_camera).unwrap();
    let export = crate::project_semantic_alignment_export(
        std::slice::from_ref(&frame),
        &ledger,
        &std::collections::HashMap::new(),
        &crate::palette::AlignmentPalette::light(),
    )
    .unwrap();
    for body in expected
        .primitives
        .iter()
        .filter(|primitive| primitive.shape != CpuPrimitiveShape::TextLabel)
    {
        assert!(export.display.rectangles.iter().any(|rectangle| {
            (f64::from(rectangle.x) - body.x).abs() < 1e-4
                && (f64::from(rectangle.y) - body.y).abs() < 1e-4
                && (f64::from(rectangle.width) - body.width).abs() < 1e-4
                && (f64::from(rectangle.height) - body.height).abs() < 1e-4
        }));
    }
    assert!(
        export
            .display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.y < 96.0
                && rectangle.color == Rgba::new(0.48, 0.48, 0.48, 1.0))
    );
    assert!(
        expected.primitives.iter().any(|primitive| matches!(
            primitive.component,
            rgv_alignment_scene::AlignmentComponent::ReadSpan
                | rgv_alignment_scene::AlignmentComponent::AlignedSegment { .. }
        )),
        "fixture must project a biological read span: {:?}",
        expected.primitives
    );
    let mut next = frame.clone();
    Arc::make_mut(&mut next.lanes)[0].scene_contracts_current = false;
    assert!(
        crate::project_semantic_alignment_export(
            &[next],
            &ledger,
            &std::collections::HashMap::new(),
            &crate::palette::AlignmentPalette::light(),
        )
        .is_err()
    );
    let themed_frame = frame.clone();
    let retained = crate::project_semantic_alignment_export(
        &[frame],
        &ledger,
        &std::collections::HashMap::new(),
        &crate::palette::AlignmentPalette::light(),
    )
    .unwrap();
    assert_eq!(retained.display, export.display);

    // The export honours the active palette: the same geometry, recoloured by the palette that
    // is passed in, for summary and read primitives alike.
    for palette in [
        AlignmentPalette::dark(),
        AlignmentPalette::light_color_blind(),
        AlignmentPalette::dark_color_blind(),
    ] {
        let themed = crate::project_semantic_alignment_export(
            std::slice::from_ref(&themed_frame),
            &ledger,
            &std::collections::HashMap::new(),
            &palette,
        )
        .unwrap();
        assert_eq!(
            themed.display.rectangles.len(),
            export.display.rectangles.len()
        );
        let mut recoloured = 0_usize;
        for (themed, light) in themed
            .display
            .rectangles
            .iter()
            .zip(&export.display.rectangles)
        {
            assert_eq!(
                (
                    themed.x,
                    themed.y,
                    themed.width,
                    themed.height,
                    themed.shape
                ),
                (light.x, light.y, light.width, light.height, light.shape)
            );
            recoloured += usize::from(themed.color != light.color);
        }
        assert!(recoloured > 0);
        let [r, g, b, a] = palette.color(PaletteToken::SUMMARY_DEPTH);
        let depth_is_themed = themed
            .display
            .rectangles
            .iter()
            .any(|rectangle| rectangle.y < 96.0 && rectangle.color == Rgba::new(r, g, b, a));
        assert!(depth_is_themed);
        // Every rectangle whose Light colour is a palette slot shows that slot's colour in the
        // palette that was passed in (several slots can share one Light colour).
        let slot_color = |palette: &AlignmentPalette, slot: u8| {
            palette.resolve(crate::palette::PaletteColor::decode(slot, 0))
        };
        let light_palette = AlignmentPalette::light();
        for (themed, light) in themed
            .display
            .rectangles
            .iter()
            .zip(&export.display.rectangles)
        {
            let light = [
                light.color.red,
                light.color.green,
                light.color.blue,
                light.color.alpha,
            ];
            let candidates = (1..PaletteToken::END.index())
                .filter(|slot| {
                    slot_color(&light_palette, *slot).map(f32::to_bits) == light.map(f32::to_bits)
                })
                .map(|slot| slot_color(&palette, slot))
                .collect::<Vec<_>>();
            let actual = [
                themed.color.red,
                themed.color.green,
                themed.color.blue,
                themed.color.alpha,
            ];
            assert!(
                candidates.is_empty() || candidates.contains(&actual),
                "{actual:?} not in {candidates:?}"
            );
        }
    }
}

#[test]
fn packed_modifications_match_the_dominant_cpu_oracle_for_live_lane_options() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let camera = camera(scene.prepared_interval());
    let page = scene
        .summary_pages()
        .iter()
        .find(|page| page.key().family() == SummaryPageFamily::Modifications)
        .unwrap();
    assert!(page.primitives().iter().any(|primitive| {
        matches!(
            primitive.component,
            SummarySemanticComponent::Modification { bin, .. } if bin.modified_calls == 0
        )
    }));
    let PackedAlignmentSummaryPage::Modifications {
        instances,
        metadata,
    } = PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::modifications(
        page,
    ))
    .unwrap()
    else {
        unreachable!();
    };
    assert_eq!(
        metadata.codes(),
        &[
            ModificationCode::Letter(b'a'),
            ModificationCode::Letter(b'h'),
            ModificationCode::Letter(b'm'),
        ]
    );
    assert!(
        instances
            .iter()
            .filter(|instance| instance.shape == 0)
            .all(|instance| { instance.modified_calls > 0 && instance.canonical_observations > 0 })
    );
    assert_eq!(metadata.background_range().len(), 1);
    assert_eq!(metadata.raw_range().len(), 3);
    assert_eq!(metadata.weighted_range().len(), 3);

    let cases = [
        (false, Vec::new(), Some(ModificationCode::Letter(b'a'))),
        (
            false,
            vec![ModificationCode::Letter(b'a')],
            Some(ModificationCode::Letter(b'h')),
        ),
        (true, Vec::new(), Some(ModificationCode::Letter(b'm'))),
        (
            true,
            vec![ModificationCode::Letter(b'm')],
            Some(ModificationCode::Letter(b'h')),
        ),
        (
            true,
            vec![
                ModificationCode::Letter(b'm'),
                ModificationCode::Letter(b'h'),
            ],
            Some(ModificationCode::Letter(b'a')),
        ),
        (
            true,
            vec![
                ModificationCode::Letter(b'm'),
                ModificationCode::Letter(b'h'),
                ModificationCode::Letter(b'a'),
            ],
            None,
        ),
    ];
    for (probability_weighted, hidden_codes, expected_code) in cases {
        let options = CpuModificationProjectionOptions {
            probability_weighted,
            hidden_codes: hidden_codes.clone(),
        };
        let cpu = CpuSummaryProjector::project_dominant_modifications(&scene, 1, camera, &options)
            .unwrap();
        let range = if probability_weighted {
            metadata.weighted_range()
        } else {
            metadata.raw_range()
        };
        let hidden_mask = metadata.hidden_code_mask(&hidden_codes);
        let mut gpu = instances[range.start as usize..range.end as usize]
            .iter()
            .filter_map(|instance| {
                project_alignment_summary_modification_instance(
                    *instance,
                    page.interval().start(),
                    camera,
                    probability_weighted,
                    hidden_mask,
                )
            })
            .collect::<Vec<_>>();
        let mut cpu = cpu
            .primitives
            .iter()
            .filter_map(|primitive| {
                let CpuSummaryProjectedComponent::Modification {
                    series,
                    bin,
                    fraction_bits,
                } = primitive.component
                else {
                    return None;
                };
                Some((primitive, series, bin, f64::from_bits(fraction_bits)))
            })
            .collect::<Vec<_>>();
        gpu.sort_by(|left, right| left.bounds.x.total_cmp(&right.bounds.x));
        cpu.sort_by(|left, right| left.0.x.total_cmp(&right.0.x));
        assert_eq!(gpu.len(), cpu.len());
        assert_eq!(
            gpu.first()
                .map(|projected| metadata.codes()[projected.code_index as usize]),
            expected_code
        );
        for (gpu, (cpu, series, bin, fraction)) in gpu.iter().zip(cpu) {
            assert_eq!(metadata.codes()[gpu.code_index as usize], series.code);
            assert_eq!(gpu.modified_calls, bin.modified_calls);
            assert_eq!(gpu.canonical_observations, bin.canonical_observations);
            assert_near(gpu.fraction, fraction);
            assert_near(gpu.bounds.x, cpu.x);
            assert_near(gpu.bounds.y, cpu.y);
            assert_near(gpu.bounds.width, cpu.width);
            assert_near(gpu.bounds.height, cpu.height);
        }
    }

    let raw_style = summary_draw_style(
        &AlignmentSummaryGpuLaneOptions::Modifications(modification_options(
            false,
            vec![ModificationCode::Letter(b'a')],
            ModifiedBaseColorMode::ByType,
        )),
        AlignmentSummaryGpuPageKey::modifications(page).unwrap(),
        &GpuSummaryPageMetadata::Modifications(metadata.clone()),
        instances.len() as u32,
    )
    .unwrap();
    assert_eq!(raw_style.background_range, 0..0);
    assert_eq!(raw_style.foreground_range, metadata.raw_range());
    assert_eq!(
        raw_style.hidden_modification_mask,
        metadata.hidden_code_mask(&[ModificationCode::Letter(b'a')])
    );
    let weighted_background_style = summary_draw_style(
        &AlignmentSummaryGpuLaneOptions::Modifications(modification_options(
            true,
            Vec::new(),
            ModifiedBaseColorMode::ModifiedVsUnmodified,
        )),
        AlignmentSummaryGpuPageKey::modifications(page).unwrap(),
        &GpuSummaryPageMetadata::Modifications(metadata.clone()),
        instances.len() as u32,
    )
    .unwrap();
    assert_eq!(
        weighted_background_style.background_range,
        metadata.background_range()
    );
    assert_eq!(
        weighted_background_style.foreground_range,
        metadata.weighted_range()
    );
}

#[test]
fn packed_junctions_have_unique_tile_ownership_and_match_the_stable_cpu_oracle() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(10).unwrap());
    let camera = camera(scene.prepared_interval());
    let mut options = sashimi_options(
        JunctionDisplayMode::Expanded,
        AlignmentSashimiScalePolicy::Fixed(1.0),
    );
    options.projection.scale_maximum = Some(1.0);
    options.projection.show_labels = false;
    let cpu = CpuSummaryProjector::project(
        &scene,
        1,
        camera,
        &CpuSummaryProjectionOptions::Junctions(options.projection),
    )
    .unwrap();

    let mut entry_count = 0_usize;
    let mut gpu = Vec::new();
    for page in scene
        .summary_pages()
        .iter()
        .filter(|page| page.key().family() == SummaryPageFamily::Junctions)
    {
        let PackedAlignmentSummaryPage::Junctions {
            instances,
            metadata,
        } = PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::junctions(
            page,
        ))
        .unwrap()
        else {
            unreachable!();
        };
        entry_count = entry_count.saturating_add(metadata.entries().len());
        for entry in metadata.entries() {
            assert_eq!(
                entry.expanded_row(),
                scene
                    .summaries()
                    .expanded_junction_row(entry.evidence().start, entry.evidence().end)
                    .unwrap()
            );
        }
        gpu.extend(instances.iter().filter_map(|instance| {
            project_alignment_summary_junction_instance(
                *instance,
                page.interval().start(),
                camera,
                1.0,
                1.0,
                scene.summaries().expanded_junction_row_count(),
                options.projection.minimum_depth,
                options.projection.minimum_flank,
                options.projection.display_mode,
            )
            .filter(|projected| {
                projected.component != AlignmentSummaryJunctionComponentToken::Annotation
            })
        }));
    }
    assert_eq!(entry_count, 1, "one biological junction has one owner page");

    let mut cpu = cpu
        .primitives
        .iter()
        .filter(|primitive| {
            matches!(
                primitive.component,
                CpuSummaryProjectedComponent::Junction { .. }
            )
        })
        .collect::<Vec<_>>();
    gpu.sort_by(|left, right| {
        left.bounds
            .x
            .total_cmp(&right.bounds.x)
            .then_with(|| left.bounds.y.total_cmp(&right.bounds.y))
    });
    cpu.sort_by(|left, right| {
        left.x
            .total_cmp(&right.x)
            .then_with(|| left.y.total_cmp(&right.y))
    });
    assert_eq!(gpu.len(), cpu.len());
    for (gpu, cpu) in gpu.iter().zip(cpu) {
        assert_near(gpu.bounds.x, cpu.x);
        assert_near(gpu.bounds.y, cpu.y);
        assert_near(gpu.bounds.width, cpu.width);
        assert_near(gpu.bounds.height, cpu.height);
    }
}

#[test]
fn junction_arcs_keep_their_unclamped_span_when_an_end_is_off_screen() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(10).unwrap());
    let mut options = sashimi_options(
        JunctionDisplayMode::Collapsed,
        AlignmentSashimiScalePolicy::Fixed(1.0),
    );
    options.projection.scale_maximum = Some(1.0);
    options.projection.show_labels = false;

    let mut packed = Vec::new();
    for page in scene
        .summary_pages()
        .iter()
        .filter(|page| page.key().family() == SummaryPageFamily::Junctions)
    {
        let PackedAlignmentSummaryPage::Junctions {
            instances,
            metadata,
        } = PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::junctions(
            page,
        ))
        .unwrap()
        else {
            unreachable!();
        };
        for entry in metadata.entries() {
            packed.push((entry.evidence(), page.interval().start(), instances.clone()));
        }
    }
    let (evidence, page_start, instances) = packed.pop().unwrap();
    let junction_bases = evidence.end - evidence.start;
    assert!(junction_bases >= 2);

    // The camera ends one base into the junction, so its right end is off-screen.
    let prepared = scene.prepared_interval();
    let cut = evidence.start + 1;
    assert!(prepared.start() < cut);
    let camera = camera(GenomicInterval::new(prepared.contig(), prepared.start(), cut).unwrap());
    let clip = camera.clip();
    let points_per_base = 1.0 / camera.bases_per_pixel();
    let expected_x = clip.x + (evidence.start as f64 - camera.exact_start()) * points_per_base;
    let expected_width = (evidence.end - evidence.start) as f64 * points_per_base;

    let gpu = instances
        .iter()
        .filter_map(|instance| {
            project_alignment_summary_junction_instance(
                *instance,
                page_start,
                camera,
                1.0,
                1.0,
                1,
                options.projection.minimum_depth,
                options.projection.minimum_flank,
                options.projection.display_mode,
            )
        })
        .filter(|projected| {
            projected.component != AlignmentSummaryJunctionComponentToken::Annotation
        })
        .collect::<Vec<_>>();
    assert!(!gpu.is_empty());
    for projected in &gpu {
        assert_near(projected.bounds.x, expected_x);
        assert_near(projected.bounds.width, expected_width);
        // The far end is beyond the pane rather than snapped to its edge: the pane edge
        // cuts the arc one base into its span, still on the rising flank.
        assert!(projected.bounds.x + projected.bounds.width > clip.x + clip.width + 1.0);
        assert_near(
            (clip.x + clip.width - projected.bounds.x) / projected.bounds.width,
            1.0 / junction_bases as f64,
        );
    }

    let cpu = CpuSummaryProjector::project(
        &scene,
        1,
        camera,
        &CpuSummaryProjectionOptions::Junctions(options.projection),
    )
    .unwrap();
    let cpu = cpu
        .primitives
        .iter()
        .filter(|primitive| {
            matches!(
                primitive.component,
                CpuSummaryProjectedComponent::Junction { evidence: candidate, .. }
                    if candidate == evidence
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(cpu.len(), gpu.len());
    for primitive in cpu {
        assert_near(primitive.x, expected_x);
        assert_near(primitive.width, expected_width);
        assert!(gpu.iter().any(|projected| {
            (projected.bounds.y - primitive.y).abs() < 1.0e-4
                && (projected.bounds.height - primitive.height).abs() < 1.0e-4
        }));
    }
}

#[test]
fn entering_junction_projects_its_true_extent_like_the_cpu_projector() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    // The fixture junction spans 15..18. Preparing summaries from 16 leaves its owner tile
    // out of the root, so tile 1 (16..32) carries it as an entering junction.
    let scene = test_snapshot_over(&ledger, None, NonZeroU64::new(16).unwrap(), 16, None);
    let camera = camera(scene.prepared_interval());
    assert_eq!(scene.prepared_interval().start(), 16);
    let mut options = sashimi_options(
        JunctionDisplayMode::Collapsed,
        AlignmentSashimiScalePolicy::Fixed(1.0),
    );
    options.projection.scale_maximum = Some(1.0);
    options.projection.show_labels = false;

    let mut gpu = Vec::new();
    let mut entries = Vec::new();
    for page in scene
        .summary_pages()
        .iter()
        .filter(|page| page.key().family() == SummaryPageFamily::Junctions)
    {
        let PackedAlignmentSummaryPage::Junctions {
            instances,
            metadata,
        } = PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::junctions(
            page,
        ))
        .unwrap()
        else {
            unreachable!();
        };
        for entry in metadata.entries() {
            entries.push((*entry, page.interval().start()));
        }
        // The instance carries the true start as a negative page-relative offset, and
        // hover/selection uniforms reproduce the same bits.
        let (span, _, present) = junction_interaction_uniform(
            Some(AlignmentSummaryJunctionInteraction {
                start: 15,
                end: 18,
                component: AlignmentSummaryJunctionComponentToken::Positive,
            }),
            page.interval().start(),
        );
        assert!(present);
        for instance in &instances {
            assert_eq!(
                span.map(f32::to_bits),
                [-1.0, 0.0, 2.0, 0.0].map(f32::to_bits)
            );
            let offsets = [instance.start_offset, instance.end_offset].concat();
            assert_eq!(
                bytemuck::cast_slice::<f32, u32>(&offsets),
                span.map(f32::to_bits)
            );
        }
        gpu.extend(instances.iter().filter_map(|instance| {
            project_alignment_summary_junction_instance(
                *instance,
                page.interval().start(),
                camera,
                1.0,
                1.0,
                1,
                options.projection.minimum_depth,
                options.projection.minimum_flank,
                options.projection.display_mode,
            )
            .filter(|projected| {
                projected.component != AlignmentSummaryJunctionComponentToken::Annotation
            })
        }));
    }
    assert_eq!(entries.len(), 1);
    let (entry, page_start) = entries[0];
    assert_eq!(
        (entry.evidence().start, entry.evidence().end, page_start),
        (15, 18, 16)
    );
    // The page-local overlap span stays clipped to the tile for range queries.
    assert_eq!((entry.start_offset(), entry.end_offset()), (0, 2));

    let cpu = CpuSummaryProjector::project(
        &scene,
        1,
        camera,
        &CpuSummaryProjectionOptions::Junctions(options.projection),
    )
    .unwrap();
    let cpu = cpu
        .primitives
        .iter()
        .filter(|primitive| {
            matches!(
                primitive.component,
                CpuSummaryProjectedComponent::Junction { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(!gpu.is_empty());
    assert_eq!(gpu.len(), cpu.len());
    let clip = camera.clip();
    let points_per_base = 1.0 / camera.bases_per_pixel();
    let expected_x = clip.x + (15.0 - camera.exact_start()) * points_per_base;
    for projected in &gpu {
        // Arc endpoints are the true genomic start and end; the start is left of the pane.
        assert_near(projected.bounds.x, expected_x);
        assert!(projected.bounds.x < clip.x);
        assert_near(projected.bounds.width, 3.0 * points_per_base);
        let right = projected.bounds.x + projected.bounds.width;
        assert!(cpu.iter().any(|primitive| {
            (primitive.x - projected.bounds.x).abs() < 1.0e-4
                && (primitive.x + primitive.width - right).abs() < 1.0e-4
                && (primitive.y - projected.bounds.y).abs() < 1.0e-4
                && (primitive.height - projected.bounds.height).abs() < 1.0e-4
        }));
    }
}

#[test]
fn junction_arc_stroke_uses_perpendicular_distance_on_steep_flanks() {
    // A narrow, tall arc: 20 points wide, 40 high, stroke 4 (the picking stroke for this
    // height), sampled a tenth of the way along where the centre line is steep.
    let (width, height) = (20.0_f64, 40.0_f64);
    let stroke = f64::from(crate::junction_arc_stroke_width(height as f32));
    assert_near(stroke, 4.0);
    let fraction = 0.1;
    let curve = junction_arc_curve_offset(fraction, height, stroke);
    let slope = 8.0 * (height - stroke) * (fraction - 0.5) / width;
    assert!(slope.abs() > 5.0);

    // Four points below the centre line is outside a vertical-distance stroke, but well
    // inside the perpendicular one.
    let vertical = 4.0;
    assert!(vertical > stroke * 0.5 + 1.0);
    for downward in [false, true] {
        let local_y = if downward {
            height - curve - vertical
        } else {
            curve + vertical
        };
        let distance =
            junction_arc_stroke_distance(fraction, local_y, width, height, stroke, downward);
        assert_near(distance, vertical / (1.0 + slope * slope).sqrt());
        assert_near(junction_arc_stroke_coverage(distance, stroke), 1.0);

        // The rendered stroke and the picking distance are the same function.
        let rectangle = RectPrimitive {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
            object_id: 0,
            shape: if downward {
                RectShape::JunctionArcDown
            } else {
                RectShape::JunctionArcUp
            },
        };
        let picked =
            junction_arc_distance_to_point(rectangle, (fraction * width) as f32, local_y as f32)
                .unwrap();
        assert!((f64::from(picked) - distance).abs() < 1.0e-4);
    }

    // At the apex the curve is flat and both measures agree.
    let apex = junction_arc_curve_offset(0.5, height, stroke);
    assert_near(
        junction_arc_stroke_distance(0.5, apex + 1.5, width, height, stroke, false),
        1.5,
    );
    assert_near(
        junction_arc_stroke_coverage(stroke * 0.5 + 1.0, stroke),
        0.0,
    );
}

#[test]
fn junction_arc_curve_offset_follows_the_fragment_parabola() {
    // Height 20, stroke 2: the centre line runs from the flat edge + 1 at the apex to the
    // baseline - 1 at either end.
    assert_near(junction_arc_curve_offset(0.5, 20.0, 2.0), 1.0);
    assert_near(junction_arc_curve_offset(0.0, 20.0, 2.0), 19.0);
    assert_near(junction_arc_curve_offset(1.0, 20.0, 2.0), 19.0);
    assert_near(
        junction_arc_curve_offset(0.25, 20.0, 2.0),
        1.0 + 18.0 * 0.25,
    );
    assert_near(junction_arc_curve_offset(7.0, 20.0, 2.0), 19.0);
}

#[test]
fn junction_range_index_matches_exhaustive_scan_and_prunes_narrow_queries() {
    let entries = (0_u64..4_096)
        .map(|index| {
            let start_offset = index.saturating_mul(10);
            AlignmentSummaryJunctionEntry {
                start_offset,
                end_offset: start_offset.saturating_add(5 + index % 7),
                evidence: rgv_model::JunctionEvidence {
                    start: start_offset,
                    end: start_offset.saturating_add(5 + index % 7),
                    depth: (index % 19 + 1) as u32,
                    minimum_left_flank: (index % 11 + 1) as u32,
                    minimum_right_flank: (index % 13 + 1) as u32,
                    ..rgv_model::JunctionEvidence::default()
                },
                expanded_row: (index % 23) as u32,
                annotation: None,
            }
        })
        .collect::<Vec<_>>();
    let index = AlignmentSummaryJunctionRangeIndex::build(&entries);

    let exhaustive = |start: u64, end: u64, minimum_depth: u32, minimum_flank: u32| {
        let mut result = JunctionRangeQuery::default();
        for entry in &entries {
            if entry.start_offset < end
                && entry.end_offset > start
                && entry.evidence.depth >= minimum_depth
                && entry.evidence.minimum_left_flank >= minimum_flank
                && entry.evidence.minimum_right_flank >= minimum_flank
            {
                result.matched = result.matched.saturating_add(1);
                result.maximum_depth = result.maximum_depth.max(entry.evidence.depth);
                result.maximum_expanded_row = result.maximum_expanded_row.max(entry.expanded_row);
            }
        }
        result
    };

    for (start, end, minimum_depth, minimum_flank) in [
        (0, 40_960, 0, 0),
        (0, 1, 1, 1),
        (9_997, 10_111, 7, 4),
        (20_000, 20_006, 1, 1),
        (30_000, 32_000, 18, 10),
        (40_950, 41_000, 3, 2),
        (1_000, 1_000, 1, 1),
    ] {
        let indexed = index.query(&entries, start, end, minimum_depth, minimum_flank);
        let expected = exhaustive(start, end, minimum_depth, minimum_flank);
        assert_eq!(indexed.matched, expected.matched);
        assert_eq!(indexed.maximum_depth, expected.maximum_depth);
        assert_eq!(indexed.maximum_expanded_row, expected.maximum_expanded_row);
    }

    let narrow = index.query(&entries, 20_000, 20_006, 1, 1);
    assert!(narrow.matched > 0);
    assert!(
        narrow.examined_entries < entries.len() / 16,
        "narrow query examined {} of {} entries",
        narrow.examined_entries,
        entries.len()
    );
}

#[test]
fn shared_sashimi_scales_are_coherent_within_but_never_across_panels() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let group = AlignmentCoherenceGroupId::new(7);
    let options = sashimi_options(
        JunctionDisplayMode::Collapsed,
        AlignmentSashimiScalePolicy::Shared(group),
    );
    let full = camera(scene.prepared_interval());
    let empty = camera(GenomicInterval::new(ContigId::new(0), 0, 5).unwrap());

    let isolated = [
        sashimi_request(&scene, 1, full, 10, 1, options),
        sashimi_request(&scene, 1, empty, 20, 2, options),
    ];
    let isolated_pages = isolated
        .iter()
        .map(|request| summary_requests_for_scene(request, true))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let metrics = resolve_junction_request_metrics(&isolated, &isolated_pages).unwrap();
    assert_near(f64::from(metrics[0].unwrap().resolved_scale_maximum), 1.0);
    assert!(metrics[1].unwrap().resolved_scale_maximum <= f32::EPSILON);

    let coherent = [
        sashimi_request(&scene, 2, full, 10, 1, options),
        sashimi_request(&scene, 2, empty, 10, 2, options),
    ];
    let coherent_pages = coherent
        .iter()
        .map(|request| summary_requests_for_scene(request, true))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let metrics = resolve_junction_request_metrics(&coherent, &coherent_pages).unwrap();
    assert_near(f64::from(metrics[1].unwrap().resolved_scale_maximum), 1.0);
}

#[test]
fn native_sashimi_picking_and_hover_capture_reuse_persistent_geometry() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 12 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let camera = camera(GenomicInterval::new(ContigId::new(0), 10, 25).unwrap());
    let options = sashimi_options(
        JunctionDisplayMode::Collapsed,
        AlignmentSashimiScalePolicy::Fixed(1.0),
    );
    let first_requests = [sashimi_request(&scene, 1, camera, 31, 41, options)];
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (first, first_stats, _) = publish_bundled_summary_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        Vec::new(),
        1,
        90,
    );
    assert!(first_stats.geometry_uploaded_bytes > 0);
    assert!(first.has_junction_lane(31, 41));
    let labels = renderer.junction_labels(&first);
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].text, "1");
    assert_eq!(
        (labels[0].coherence_scope, labels[0].lane_identity),
        (31, 41)
    );
    render_test_frame(&device, &queue, &renderer, &first).unwrap();

    let page = scene
        .summary_pages()
        .iter()
        .find(|page| {
            page.key().family() == SummaryPageFamily::Junctions && !page.primitives().is_empty()
        })
        .unwrap();
    let PackedAlignmentSummaryPage::Junctions {
        instances,
        metadata,
    } = PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::junctions(page))
        .unwrap()
    else {
        unreachable!();
    };
    let instance = *instances
        .iter()
        .find(|instance| {
            instance.identity[1] == AlignmentSummaryJunctionComponentToken::Positive as u32
        })
        .unwrap();
    let projected = project_alignment_summary_junction_instance(
        instance,
        page.interval().start(),
        camera,
        1.0,
        1.0,
        1,
        1,
        0,
        JunctionDisplayMode::Collapsed,
    )
    .unwrap();
    let hit = renderer
        .hit_test_junction_lane(
            &first,
            31,
            41,
            projected.bounds.x + projected.bounds.width * 0.5,
            projected.bounds.y,
            4.0,
        )
        .unwrap();
    assert_eq!(hit.coherence_scope, 31);
    assert_eq!(hit.lane_identity, 41);
    assert_eq!(hit.evidence, metadata.entries()[0].evidence());
    assert_eq!(
        hit.component,
        AlignmentSummaryJunctionComponentToken::Positive
    );

    let authority = first.content_authority().clone();
    let evidence = hit.evidence;
    let mut hovered = sashimi_request(&scene, 2, camera, 31, 41, options);
    hovered.overlays.hovered_junction = Some(AlignmentSummaryJunctionInteraction {
        start: evidence.start,
        end: evidence.end,
        component: hit.component,
    });
    let resident_pages = renderer.resident_page_count();
    let (second, stats) = renderer
        .capture_camera_frame(&device, &queue, &authority, &[hovered], &ledger)
        .unwrap();
    assert_eq!(stats.geometry_uploaded_bytes, 0);
    assert_eq!(renderer.resident_page_count(), resident_pages);
    assert!(second.has_junction_lane(31, 41));
    render_test_frame(&device, &queue, &renderer, &second).unwrap();
}

#[test]
fn canonical_gpu_pages_are_clipped_to_the_snapshot_prepared_interval() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let prepared = GenomicInterval::new(ContigId::new(0), 12, 20).unwrap();
    let scene = test_snapshot_with_prepared_interval(&ledger, Some(prepared));
    let camera = camera(GenomicInterval::new(ContigId::new(0), 0, 100).unwrap());
    let request = request(&scene, 1, camera, lane_options(false));
    let covered = summary_covered_interval(&request).unwrap();
    assert_eq!(covered, prepared);
    let clip = summary_covered_clip_x(camera, covered);
    assert_near(f64::from(clip[0]), 30.72);
    assert_near(f64::from(clip[1]), 51.2);

    let cpu = CpuSummaryProjector::project(
        &scene,
        1,
        camera,
        &CpuSummaryProjectionOptions::Depth(CpuDepthProjectionOptions {
            scale_maximum: Some(1),
            transform: CoverageScaleTransform::Linear,
            show_center_line: false,
            selected_position: None,
        }),
    )
    .unwrap();
    let depth_page = scene
        .summary_pages()
        .iter()
        .find(|page| page.key().family() == SummaryPageFamily::Depth)
        .unwrap();
    let PackedAlignmentSummaryPage::Depth { instances, .. } =
        PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::depth(depth_page))
            .unwrap()
    else {
        unreachable!();
    };
    let mut gpu = instances
        .iter()
        .filter_map(|instance| {
            projected_horizontal_exact(
                instance.start_offset() as f64,
                instance.end_offset() as f64,
                depth_page.interval().start(),
                camera,
                [f64::from(clip[0]), f64::from(clip[1])],
            )
        })
        .collect::<Vec<_>>();
    let mut cpu = cpu
        .primitives
        .iter()
        .filter_map(|primitive| {
            matches!(
                primitive.component,
                CpuSummaryProjectedComponent::Depth { .. }
            )
            .then_some((primitive.x, primitive.width))
        })
        .collect::<Vec<_>>();
    gpu.sort_by(|left, right| left.0.total_cmp(&right.0));
    cpu.sort_by(|left, right| left.0.total_cmp(&right.0));
    assert_eq!(gpu.len(), cpu.len());
    for ((gpu_x, gpu_width), (cpu_x, cpu_width)) in gpu.into_iter().zip(cpu) {
        assert_near(gpu_x, cpu_x);
        assert_near(gpu_width, cpu_width);
        assert!(gpu_x >= f64::from(clip[0]));
        assert!(gpu_x + gpu_width <= f64::from(clip[1]) + 1.0);
    }

    let visible = summary_requests_for_scene(&request, true).unwrap();
    assert_eq!(visible.len(), 1);
    let uniform = summary_uniform(
        camera,
        [256.0, 96.0],
        depth_page.interval().start(),
        clip,
        1,
        0.0,
        CoverageScaleTransform::Linear,
        false,
        CoverageAlleleInterpretation::GenericReferenceAware,
        SummaryPageFamily::Depth,
        0,
        ModifiedBaseColorMode::ByType,
        1.0,
        1.0,
        1,
        0,
        0,
        JunctionDisplayMode::Collapsed,
        AlignmentSummaryFrameOverlays::default(),
    );
    assert_eq!(uniform.covered_clip_x, clip);
}

#[test]
fn overlays_require_scene_overlap_and_selected_marker_matches_cpu_geometry() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let viewport = Viewport::new(scene.prepared_interval(), 256.0)
        .unwrap()
        .pan_pixels(1.28, 1_000)
        .unwrap();
    let live_camera = CpuSummaryCamera::from_viewport(
        viewport,
        rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 96.0,
        },
    )
    .unwrap();
    let mut options = lane_options(false);
    // The CPU oracle uses the enclosing integer camera interval as marker authority. A
    // fractional pan can therefore retain the partially scissored leading-base marker.
    options.depth.selected_position = Some(live_camera.interval().start());
    let cpu = CpuSummaryProjector::project(
        &scene,
        1,
        live_camera,
        &CpuSummaryProjectionOptions::Depth(options.depth),
    )
    .unwrap();
    let cpu_marker = cpu
        .primitives
        .iter()
        .find(|primitive| {
            matches!(
                primitive.component,
                CpuSummaryProjectedComponent::CoverageSelectedPosition { .. }
            )
        })
        .unwrap();
    let live_request = request(&scene, 1, live_camera, options);
    let mut overlays = Vec::new();
    append_summary_overlays(&live_request, &mut overlays);
    assert_eq!(overlays.len(), 1);
    assert_near(f64::from(overlays[0].origin[0]), cpu_marker.x);
    assert_near(f64::from(overlays[0].origin[1]), cpu_marker.y);
    assert_near(f64::from(overlays[0].size[0]), cpu_marker.width);
    assert_near(f64::from(overlays[0].size[1]), cpu_marker.height);

    let disjoint_camera = camera(GenomicInterval::new(ContigId::new(0), 100, 200).unwrap());
    let disjoint = request(&scene, 2, disjoint_camera, options);
    assert_eq!(summary_overlay_count(&disjoint), 0);
    let mut disjoint_overlays = Vec::new();
    append_summary_overlays(&disjoint, &mut disjoint_overlays);
    assert!(disjoint_overlays.is_empty());
    assert!(
        summary_requests_for_scene(&disjoint, true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn batch_admission_is_atomic_and_all_device_pages_are_ledger_accounted() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let owned = unique_summary_requests(
        &[request(
            &scene,
            1,
            camera(scene.prepared_interval()),
            lane_options(true),
        )],
        true,
    )
    .unwrap();
    let borrowed = borrowed_summary_requests(&owned);
    assert_eq!(borrowed.len(), 2);
    let baseline = ledger.snapshot().unwrap().charged;
    ledger.set_budget(baseline.saturating_add(1)).unwrap();
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    assert!(matches!(
        renderer.admit_pages(&device, &queue, &borrowed, &ledger, 60),
        Err(AlignmentSummaryGpuPageAdmissionError::Ledger(
            LedgerError::BudgetExceeded { .. }
        ))
    ));
    assert_eq!(renderer.resident_page_count(), 0);

    ledger.set_budget(64 << 20).unwrap();
    let admission = renderer
        .admit_pages(&device, &queue, &borrowed, &ledger, 61)
        .unwrap();
    assert_eq!(admission.admitted_pages, 2);
    assert!(admission.uploaded_bytes > 0);
    assert_eq!(renderer.resident_page_count(), 2);
    assert_eq!(
        renderer.cache.reclaim_index.len(),
        renderer.cache.pages.len()
    );
    assert_eq!(
        renderer
            .cache
            .page_table_allocation
            .as_ref()
            .unwrap()
            .snapshot()
            .unwrap()
            .charged_capacity,
        summary_page_table_retained_bytes(renderer.cache.page_table_capacity).unwrap()
    );
    let after = ledger.snapshot().unwrap();
    assert!(after.charged > baseline);
    assert!(
        after.records.iter().any(|record| {
            record.class == AllocationClass::GpuPage && record.charged_capacity > 0
        })
    );
}

#[test]
fn fractional_camera_frames_reuse_pages_and_render_with_uniform_only_updates() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let navigation_interval = GenomicInterval::new(ContigId::new(0), 0, 50).unwrap();
    let first_camera = camera(navigation_interval);
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (first, first_stats, first_admission) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 1, first_camera, lane_options(true))],
            &ledger,
            70,
        )
        .unwrap();
    assert_eq!(first_admission.admitted_pages, 2);
    assert!(first_stats.geometry_uploaded_bytes > 0);
    assert_eq!(first_stats.range_maximum_queries, 1);
    assert_eq!(first.draw_count(), 2);
    render_test_frame(&device, &queue, &renderer, &first).unwrap();
    drop(first);

    let viewport = Viewport::new(navigation_interval, 256.0)
        .unwrap()
        .pan_pixels(0.5, 100)
        .unwrap()
        .zoom_at(128.0, 0.9, 100)
        .unwrap();
    assert!(viewport.exact_start() > 0.0);
    let panned = CpuSummaryCamera::from_viewport(
        viewport,
        rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 96.0,
        },
    )
    .unwrap();
    let mut changed_options = lane_options(true);
    changed_options.depth.transform = CoverageScaleTransform::SquareRoot;
    changed_options.depth.show_center_line = true;
    changed_options.depth.selected_position = Some(12);
    let mut changed_alleles = changed_options.alleles.unwrap();
    changed_alleles.transform = CoverageScaleTransform::SquareRoot;
    changed_alleles.quality_weighted = true;
    changed_alleles.minimum_fraction = 0.25;
    changed_options.alleles = Some(changed_alleles);
    let (second, second_stats, second_admission) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 2, panned, changed_options)],
            &ledger,
            71,
        )
        .unwrap();
    assert_eq!(second_admission.admitted_pages, 0);
    assert_eq!(second_admission.already_resident_pages, 2);
    assert_eq!(second_stats.geometry_uploaded_bytes, 0);
    assert_eq!(second_stats.overlay_instances, 2);
    assert_eq!(second.overlay_draw_count(), 1);
    assert_eq!(renderer.resident_page_count(), 2);
    render_test_frame(&device, &queue, &renderer, &second).unwrap();
    renderer.discard_device_resources();
    assert_eq!(renderer.resident_page_count(), 0);
    assert!(matches!(
        render_test_frame(&device, &queue, &renderer, &second),
        Err(AlignmentSummaryGpuRenderError::DeviceGenerationMismatch { .. })
    ));
}

#[test]
fn camera_scale_and_overlay_captures_preserve_summary_authority_without_a_bundle() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 12 << 20, 16 << 20);
    let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let first_camera = camera(GenomicInterval::new(ContigId::new(0), 0, 20).unwrap());
    let first_requests = [request(&scene, 1, first_camera, lane_options(true))];
    let (first, _, _) = publish_bundled_summary_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        Vec::new(),
        500,
        600,
    );
    renderer
        .prefetch_prepared_scene_pages(&device, &queue, &first_requests, &ledger, 601)
        .unwrap();
    let authority = first.content_authority().clone();
    drop(first);
    let page_allocations = renderer
        .cache
        .pages
        .iter()
        .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
        .collect::<Vec<_>>();
    let page_table_allocation = renderer
        .cache
        .page_table_allocation
        .as_ref()
        .map(AllocationLease::id);

    let mut square_root = lane_options(true);
    square_root.depth.transform = CoverageScaleTransform::SquareRoot;
    square_root.depth.show_center_line = true;
    let mut square_root_alleles = square_root.alleles.unwrap();
    square_root_alleles.transform = CoverageScaleTransform::SquareRoot;
    square_root_alleles.minimum_fraction = 0.2;
    square_root.alleles = Some(square_root_alleles);
    let mut selected = lane_options(true);
    selected.depth.selected_position = Some(42);
    let camera_options = [
        (
            camera(GenomicInterval::new(ContigId::new(0), 5, 25).unwrap()),
            lane_options(true),
        ),
        (
            camera(GenomicInterval::new(ContigId::new(0), 15, 35).unwrap()),
            square_root,
        ),
        (
            camera(GenomicInterval::new(ContigId::new(0), 35, 55).unwrap()),
            selected,
        ),
    ];
    let repeated_camera = camera_options[2].0;
    let repeated_options = camera_options[2].1;
    let operations_before = ledger.operation_counters().unwrap();
    let renderer_operations_before = renderer.operation_counters();
    let mut presented = None;
    let mut page_pin_acquisitions = Vec::new();
    for (index, (camera, options)) in camera_options.into_iter().enumerate() {
        assert_eq!(ledger.progress().unwrap().active_replacement_bundle, None);
        let requests = [request(&scene, 2 + index as u64, camera, options)];
        let (frame, stats) = renderer
            .capture_camera_frame(&device, &queue, &authority, &requests, &ledger)
            .unwrap();
        assert_eq!(stats.geometry_uploaded_bytes, 0);
        assert_eq!(stats.page_cache_misses, 0);
        page_pin_acquisitions.push(stats.page_pin_acquisitions);
        assert_eq!(frame.content_authority(), &authority);
        assert_eq!(ledger.progress().unwrap().active_replacement_bundle, None);
        assert_eq!(
            renderer
                .cache
                .pages
                .iter()
                .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
                .collect::<Vec<_>>(),
            page_allocations
        );
        assert_eq!(
            renderer
                .cache
                .page_table_allocation
                .as_ref()
                .map(AllocationLease::id),
            page_table_allocation
        );
        let previous = presented.replace(frame);
        drop(previous);
    }
    assert!(page_pin_acquisitions[0] > 0);
    let repeated_requests = [request(&scene, 99, repeated_camera, repeated_options)];
    let (repeated, repeated_stats) = renderer
        .capture_camera_frame(&device, &queue, &authority, &repeated_requests, &ledger)
        .unwrap();
    assert_eq!(
        repeated_stats.page_pin_acquisitions, 0,
        "a contained summary frame must reuse the prior presentation's page leases"
    );
    let previous = presented.replace(repeated);
    drop(previous);
    let operations_after = ledger.operation_counters().unwrap();
    let renderer_operations_after = renderer.operation_counters();
    assert_eq!(
        operations_after.full_inventory_snapshots,
        operations_before.full_inventory_snapshots
    );
    assert_eq!(
        operations_after.allocation_records_enumerated,
        operations_before.allocation_records_enumerated
    );
    assert_eq!(
        renderer_operations_after, renderer_operations_before,
        "camera, scale, and overlay capture must not rebuild the full prepared summary manifest"
    );
    assert!(renderer.frame_buffers.len() <= 2);

    let changed_scene_set = [
        request(&scene, 7, first_camera, lane_options(true)),
        request(&scene, 8, first_camera, lane_options(true)),
    ];
    assert!(matches!(
        renderer.capture_camera_frame(&device, &queue, &authority, &changed_scene_set, &ledger,),
        Err(
            AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentSummaryGpuContentReplacementReason::SceneContent { .. }
            }
        )
    ));

    let modification_requests = [modification_request(
        &scene,
        9,
        first_camera,
        modification_options(false, Vec::new(), ModifiedBaseColorMode::ByType),
    )];
    assert!(matches!(
        renderer
            .capture_camera_frame(&device, &queue, &authority, &modification_requests, &ledger,),
        Err(
            AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentSummaryGpuContentReplacementReason::ContentContract { .. }
            }
        )
    ));
    let (replacement, replacement_stats, _) = publish_bundled_summary_frame(
        &device,
        &queue,
        &mut renderer,
        &modification_requests,
        &ledger,
        Vec::new(),
        540,
        640,
    );
    assert!(replacement_stats.geometry_uploaded_bytes > 0);
    assert!(replacement.draw_count() > 0);

    let replacement_authority = replacement.content_authority().clone();
    drop(replacement);
    renderer.discard_device_resources();
    assert!(matches!(
        renderer.capture_camera_frame(
            &device,
            &queue,
            &replacement_authority,
            &modification_requests,
            &ledger,
        ),
        Err(
            AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                reason: AlignmentSummaryGpuContentReplacementReason::DeviceGeneration { .. }
            }
        )
    ));
}

#[test]
fn compatible_summary_capture_uses_the_precharged_ring_during_an_unrelated_bundle() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 12 << 20, 16 << 20);
    let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let first_camera = camera(GenomicInterval::new(ContigId::new(0), 0, 20).unwrap());
    let first_requests = [request(&scene, 1, first_camera, lane_options(true))];
    let (first, _, _) = publish_bundled_summary_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        Vec::new(),
        900,
        901,
    );
    renderer
        .prefetch_prepared_scene_pages(&device, &queue, &first_requests, &ledger, 902)
        .unwrap();
    let authority = first.content_authority().clone();
    assert_eq!(
        renderer.frame_buffers.len(),
        ALIGNMENT_SUMMARY_CAMERA_FRAME_SLOT_COUNT
    );
    let slot_ids = renderer
        .frame_buffers
        .iter()
        .map(|slot| {
            (
                slot.uniform_allocation.as_ref().map(AllocationLease::id),
                slot.overlay_allocation.as_ref().map(AllocationLease::id),
                slot.frame_allocation.id(),
            )
        })
        .collect::<Vec<_>>();

    let mut options = lane_options(true);
    options.depth.show_center_line = true;
    options.depth.selected_position = Some(42);
    let requests = [request(
        &scene,
        2,
        camera(scene.prepared_interval()),
        options,
    )];
    let policy_revision = ledger.snapshot().unwrap().policy_revision;
    let unrelated = ledger
        .reserve_replacement_bundle(AllocationBundleSpec::new(
            policy_revision,
            Vec::new(),
            ledger.empty_retirement_claim_set(),
        ))
        .unwrap();
    let before = ledger.snapshot().unwrap();
    let allocation_ids_before = before
        .records
        .iter()
        .map(|record| record.id)
        .collect::<Vec<_>>();
    let (second, stats) = renderer
        .capture_camera_frame(&device, &queue, &authority, &requests, &ledger)
        .unwrap();
    assert!(stats.mandatory_pages > first.draw_count());
    assert_eq!(stats.overlay_instances, 2);
    assert_eq!(stats.geometry_uploaded_bytes, 0);
    assert_ne!(
        first.buffers.frame_allocation.id(),
        second.buffers.frame_allocation.id()
    );
    assert!(matches!(
        renderer.capture_camera_frame(&device, &queue, &authority, &requests, &ledger),
        Err(AlignmentSummaryGpuCameraCaptureError::FrameSlotBusy { slots: 2 })
    ));
    let after = ledger.snapshot().unwrap();
    assert_eq!(
        after.active_replacement_bundle,
        before.active_replacement_bundle
    );
    assert_eq!(after.charged, before.charged);
    assert_eq!(after.reserved, before.reserved);
    assert_eq!(
        after
            .records
            .iter()
            .map(|record| record.id)
            .collect::<Vec<_>>(),
        allocation_ids_before
    );
    assert_eq!(
        renderer
            .frame_buffers
            .iter()
            .map(|slot| (
                slot.uniform_allocation.as_ref().map(AllocationLease::id),
                slot.overlay_allocation.as_ref().map(AllocationLease::id),
                slot.frame_allocation.id(),
            ))
            .collect::<Vec<_>>(),
        slot_ids
    );
    drop(unrelated);
    drop(second);
    drop(first);
}

#[test]
fn bundled_replacement_combines_multiple_lanes_and_reuses_unchanged_geometry() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let camera = camera(scene.prepared_interval());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (old_depth, _, _) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 1, camera, lane_options(false))],
            &ledger,
            200,
        )
        .unwrap();
    let (old_alleles, _, _) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 2, camera, lane_options(true))],
            &ledger,
            201,
        )
        .unwrap();
    render_test_frame(&device, &queue, &renderer, &old_depth).unwrap();
    render_test_frame(&device, &queue, &renderer, &old_alleles).unwrap();

    let candidate_requests = [
        request(&scene, 3, camera, lane_options(false)),
        request(&scene, 4, camera, lane_options(true)),
    ];
    let snapshot = ledger.snapshot().unwrap();
    let plan = renderer
        .plan_replacement(
            &candidate_requests,
            &snapshot,
            AllocationBundleSlot::new(40),
        )
        .unwrap();
    assert_eq!(plan.first_bundle_slot(), AllocationBundleSlot::new(40));
    let mut old_frames = vec![old_depth, old_alleles];
    let retirement = renderer
        .detach_replacement_retirement(&ledger, &plan, &mut old_frames)
        .unwrap();
    assert!(old_frames.is_empty());
    let spec = AllocationBundleSpec::new(
        plan.policy_revision(),
        plan.demands().to_vec(),
        retirement.retirement_claim_set().clone(),
    );
    let bundle = ledger.reserve_replacement_bundle(spec).unwrap();
    let reserved = ledger.snapshot().unwrap();
    assert_eq!(reserved.reserved_transition, bundle.maximum_bytes());
    let staged = renderer
        .stage_replacement(
            &device,
            &queue,
            &candidate_requests,
            plan,
            &bundle,
            &ledger,
            202,
        )
        .unwrap();
    assert_eq!(staged.geometry_uploaded_bytes, 0);
    let ready = renderer.preflight_replacement_publication(staged).unwrap();
    let committed = bundle.commit(ready.actuals().to_vec()).unwrap();
    let mut retirement = Some(retirement);
    let mut leases = committed.retire_and_settle(&mut retirement).unwrap();
    let publishable = renderer.bind_replacement_leases(ready, &mut leases);
    let (candidate, stats, admission) = renderer.publish_replacement(publishable);
    assert!(leases.is_empty());
    assert_eq!(candidate.scene_revisions.len(), 2);
    assert_eq!(candidate.camera_sequences.as_ref(), [3, 4]);
    assert_eq!(candidate.draw_count(), 3);
    assert_eq!(admission.admitted_pages, 0);
    assert_eq!(stats.geometry_uploaded_bytes, 0);
    render_test_frame(&device, &queue, &renderer, &candidate).unwrap();
}

#[test]
fn bundled_summary_replacements_preserve_multi_quantum_prefetch_without_repeat_uploads() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let first_camera = camera(GenomicInterval::new(ContigId::new(0), 0, 20).unwrap());
    let first_requests = [request(&scene, 1, first_camera, lane_options(true))];
    let prepared_keys = AlignmentSummaryGpuRenderer::prepared_page_keys(&first_requests).unwrap();
    assert!(prepared_keys.len() >= 10);
    let (first, _, first_admission) = publish_bundled_summary_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        Vec::new(),
        300,
        400,
    );
    assert!(first_admission.admitted_pages < prepared_keys.len());

    let mut admitted_quanta = 0_usize;
    let mut previous_resident = renderer.resident_page_count();
    loop {
        let progress = renderer
            .prefetch_prepared_scene_pages_bounded(
                &device,
                &queue,
                &first_requests,
                &ledger,
                401 + admitted_quanta as u64,
                NonZeroUsize::new(1).unwrap(),
            )
            .unwrap();
        assert!(progress.admission.admitted_pages <= 1);
        let resident = renderer.resident_page_count();
        assert!(resident >= previous_resident);
        if progress.admission.admitted_pages > 0 {
            admitted_quanta = admitted_quanta.saturating_add(1);
            assert_eq!(resident, previous_resident.saturating_add(1));
        }
        previous_resident = resident;
        if progress.is_complete() {
            break;
        }
    }
    assert!(admitted_quanta > 2);
    assert_eq!(renderer.resident_page_count(), prepared_keys.len());
    let converged_page_ids = renderer
        .cache
        .pages
        .iter()
        .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
        .collect::<Vec<_>>();

    let repeated_requests = [request(&scene, 2, first_camera, lane_options(true))];
    let (second, second_stats, second_admission) = publish_bundled_summary_frame(
        &device,
        &queue,
        &mut renderer,
        &repeated_requests,
        &ledger,
        vec![first],
        360,
        440,
    );
    assert_eq!(second_admission.admitted_pages, 0);
    assert_eq!(second_stats.geometry_uploaded_bytes, 0);
    assert_eq!(renderer.resident_page_count(), prepared_keys.len());
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
            .collect::<Vec<_>>(),
        converged_page_ids
    );
    let no_repeat = renderer
        .prefetch_prepared_scene_pages_bounded(
            &device,
            &queue,
            &repeated_requests,
            &ledger,
            441,
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
    assert!(no_repeat.is_complete());
    assert_eq!(no_repeat.admission.admitted_pages, 0);
    assert_eq!(no_repeat.admission.uploaded_bytes, 0);

    let compatible_camera = camera(GenomicInterval::new(ContigId::new(0), 20, 40).unwrap());
    let compatible_requests = [request(&scene, 3, compatible_camera, lane_options(true))];
    let (third, third_stats, third_admission) = publish_bundled_summary_frame(
        &device,
        &queue,
        &mut renderer,
        &compatible_requests,
        &ledger,
        vec![second],
        420,
        442,
    );
    assert_eq!(third_admission.admitted_pages, 0);
    assert_eq!(third_stats.geometry_uploaded_bytes, 0);
    assert_eq!(renderer.resident_page_count(), prepared_keys.len());
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| (page.key, page.allocation.as_ref().map(AllocationLease::id)))
            .collect::<Vec<_>>(),
        converged_page_ids
    );
    render_test_frame(&device, &queue, &renderer, &third).unwrap();
}

#[test]
fn summary_prefetch_cursor_resolves_once_then_advances_by_quantum() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let scene = test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let requests = [request(
        &scene,
        1,
        camera(scene.prepared_interval()),
        lane_options(true),
    )];
    let keys: Arc<[AlignmentSummaryGpuPageKey]> =
        AlignmentSummaryGpuRenderer::prepared_page_keys(&requests)
            .unwrap()
            .into();
    let before = renderer.operation_counters();
    let mut cursor = renderer
        .begin_prepared_scene_prefetch_cursor(
            &requests,
            Arc::clone(&keys),
            ledger.progress().unwrap().budget,
        )
        .unwrap();
    let after_begin = renderer.operation_counters();
    assert_eq!(after_begin, before);

    let mut revision = 50_u64;
    while !cursor.is_complete() {
        let before_step = renderer.operation_counters();
        let progress = renderer
            .advance_prepared_scene_prefetch_cursor(
                &device,
                &queue,
                &mut cursor,
                &ledger,
                revision,
                NonZeroUsize::new(2).unwrap(),
            )
            .unwrap();
        assert!(progress.admission.requested_pages <= 2);
        let after_step = renderer.operation_counters();
        assert_eq!(
            after_step.content_manifest_builds,
            before_step.content_manifest_builds
        );
        assert_eq!(
            after_step.content_manifest_pages_enumerated,
            before_step.content_manifest_pages_enumerated
        );
        assert!(
            after_step
                .prefetch_authority_keys_inspected
                .saturating_sub(before_step.prefetch_authority_keys_inspected)
                <= 2
        );
        revision = revision.saturating_add(1);
    }
    let residency = renderer
        .finish_prepared_scene_prefetch_cursor(cursor)
        .unwrap();
    assert_eq!(residency.page_keys.as_ref(), keys.as_ref());
    assert_eq!(renderer.resident_page_count(), keys.len());
    drop(residency);

    let mut limited = renderer
        .begin_prepared_scene_prefetch_cursor(&requests, Arc::clone(&keys), 0)
        .unwrap();
    while !limited.is_complete() {
        let progress = renderer
            .advance_prepared_scene_prefetch_cursor(
                &device,
                &queue,
                &mut limited,
                &ledger,
                revision,
                NonZeroUsize::new(2).unwrap(),
            )
            .unwrap();
        assert!(progress.retention_limited);
    }
    assert!(matches!(
        renderer.finish_prepared_scene_prefetch_cursor(limited),
        Err(
            AlignmentSummaryGpuPrefetchCursorError::ResidencyBudgetLimited {
                maximum_total_pinned_bytes: 0,
                ..
            }
        )
    ));
}

#[test]
fn refused_summary_bundle_restores_exact_frames_and_cache_ownership() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let old_scene = test_snapshot(&ledger);
    let candidate_scene =
        test_snapshot_with_summary_tile_bases(&ledger, NonZeroU64::new(25).unwrap());
    let current_camera = camera(old_scene.prepared_interval());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (prior, _, _) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&old_scene, 1, current_camera, lane_options(true))],
            &ledger,
            210,
        )
        .unwrap();
    let page_keys = renderer
        .cache
        .pages
        .iter()
        .map(|page| page.key)
        .collect::<Vec<_>>();
    let page_ids = renderer
        .cache
        .pages
        .iter()
        .map(|page| page.allocation.as_ref().map(AllocationLease::id))
        .collect::<Vec<_>>();
    let table_id = renderer
        .cache
        .page_table_allocation
        .as_ref()
        .map(AllocationLease::id);
    let table_capacity = renderer.cache.page_table_capacity;
    let frame_buffer_ptrs = renderer
        .frame_buffers
        .iter()
        .map(Arc::as_ptr)
        .collect::<Vec<_>>();

    let occupied = ledger.snapshot().unwrap().charged;
    ledger.set_limits(occupied, 0, 16 << 20).unwrap();
    let candidate_camera = camera(GenomicInterval::new(ContigId::new(0), 25, 50).unwrap());
    let candidate_requests = [request(
        &candidate_scene,
        2,
        candidate_camera,
        lane_options(true),
    )];
    let snapshot = ledger.snapshot().unwrap();
    let plan = renderer
        .plan_replacement(
            &candidate_requests,
            &snapshot,
            AllocationBundleSlot::new(80),
        )
        .unwrap();
    let mut prior_frames = vec![prior];
    let foreign_ledger = AllocationLedger::new(1 << 20, 1 << 20);
    assert!(matches!(
        renderer.detach_replacement_retirement(&foreign_ledger, &plan, &mut prior_frames),
        Err(AlignmentSummaryGpuReplacementError::Bundle(
            AllocationBundleError::ForeignRetirementReference { .. }
        ))
    ));
    assert_eq!(prior_frames.len(), 1);
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>(),
        page_keys
    );
    assert_eq!(
        renderer
            .frame_buffers
            .iter()
            .map(Arc::as_ptr)
            .collect::<Vec<_>>(),
        frame_buffer_ptrs
    );
    render_test_frame(&device, &queue, &renderer, &prior_frames[0]).unwrap();
    let retirement = renderer
        .detach_replacement_retirement(&ledger, &plan, &mut prior_frames)
        .unwrap();
    assert!(prior_frames.is_empty());
    assert_eq!(retirement.retired_pages.len(), page_keys.len());
    let spec = AllocationBundleSpec::new(
        plan.policy_revision(),
        plan.demands().to_vec(),
        retirement.retirement_claim_set().clone(),
    );
    assert!(matches!(
        ledger.reserve_replacement_bundle(spec),
        Err(AllocationBundleError::SteadyBudgetExceeded { .. }
            | AllocationBundleError::TransitionOverlapBudgetExceeded { .. })
    ));
    let restored = renderer.restore_replacement_retirement(retirement).unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>(),
        page_keys
    );
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| page.allocation.as_ref().map(AllocationLease::id))
            .collect::<Vec<_>>(),
        page_ids
    );
    assert_eq!(
        renderer
            .cache
            .page_table_allocation
            .as_ref()
            .map(AllocationLease::id),
        table_id
    );
    assert_eq!(renderer.cache.page_table_capacity, table_capacity);
    assert_eq!(
        renderer
            .frame_buffers
            .iter()
            .map(Arc::as_ptr)
            .collect::<Vec<_>>(),
        frame_buffer_ptrs
    );
    render_test_frame(&device, &queue, &renderer, &restored[0]).unwrap();
}

#[test]
fn summary_bundle_rejects_first_frame_and_retired_frame_that_cannot_fit_steady_budget() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 64 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let camera = camera(scene.prepared_interval());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let requests = [request(&scene, 1, camera, lane_options(true))];
    let probe = renderer
        .plan_replacement(
            &requests,
            &ledger.snapshot().unwrap(),
            AllocationBundleSlot::new(120),
        )
        .unwrap();
    let maximum = probe
        .demands()
        .iter()
        .map(|demand| demand.maximum_bytes)
        .sum::<usize>();
    let baseline = ledger.snapshot().unwrap().charged;
    ledger
        .set_limits(
            baseline.saturating_add(maximum).saturating_sub(1),
            64 << 20,
            16 << 20,
        )
        .unwrap();
    let first_plan = renderer
        .plan_replacement(
            &requests,
            &ledger.snapshot().unwrap(),
            AllocationBundleSlot::new(120),
        )
        .unwrap();
    let first_spec = AllocationBundleSpec::new(
        first_plan.policy_revision(),
        first_plan.demands().to_vec(),
        ledger.empty_retirement_claim_set(),
    );
    assert!(matches!(
        ledger.reserve_replacement_bundle(first_spec),
        Err(AllocationBundleError::SteadyBudgetExceeded { .. })
    ));

    ledger.set_limits(64 << 20, 64 << 20, 16 << 20).unwrap();
    let first_plan = renderer
        .plan_replacement(
            &requests,
            &ledger.snapshot().unwrap(),
            AllocationBundleSlot::new(140),
        )
        .unwrap();
    let first_spec = AllocationBundleSpec::new(
        first_plan.policy_revision(),
        first_plan.demands().to_vec(),
        ledger.empty_retirement_claim_set(),
    );
    let first_bundle = ledger.reserve_replacement_bundle(first_spec).unwrap();
    let first_staged = renderer
        .stage_replacement(
            &device,
            &queue,
            &requests,
            first_plan,
            &first_bundle,
            &ledger,
            220,
        )
        .unwrap();
    let first_ready = renderer
        .preflight_replacement_publication(first_staged)
        .unwrap();
    let mut first_leases = first_bundle
        .commit(first_ready.actuals().to_vec())
        .unwrap()
        .settle()
        .unwrap();
    let first_publishable = renderer.bind_replacement_leases(first_ready, &mut first_leases);
    let (prior, _, first_admission) = renderer.publish_replacement(first_publishable);
    assert!(first_leases.is_empty());
    assert_eq!(first_admission.admitted_pages, 2);
    render_test_frame(&device, &queue, &renderer, &prior).unwrap();
    ledger.set_limits(0, 64 << 20, 16 << 20).unwrap();
    let replacement_requests = [request(&scene, 2, camera, lane_options(true))];
    let plan = renderer
        .plan_replacement(
            &replacement_requests,
            &ledger.snapshot().unwrap(),
            AllocationBundleSlot::new(160),
        )
        .unwrap();
    let mut prior_frames = vec![prior];
    let retirement = renderer
        .detach_replacement_retirement(&ledger, &plan, &mut prior_frames)
        .unwrap();
    assert!(prior_frames.is_empty());
    let spec = AllocationBundleSpec::new(
        plan.policy_revision(),
        plan.demands().to_vec(),
        retirement.retirement_claim_set().clone(),
    );
    assert!(matches!(
        ledger.reserve_replacement_bundle(spec),
        Err(AllocationBundleError::SteadyBudgetExceeded { .. })
    ));
    let restored = renderer.restore_replacement_retirement(retirement).unwrap();
    assert_eq!(restored.len(), 1);
    render_test_frame(&device, &queue, &renderer, &restored[0]).unwrap();
}

#[test]
fn mandatory_candidate_uses_transition_reserve_while_prior_frame_is_pinned() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let camera = camera(scene.prepared_interval());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (prior, _, prior_admission) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 1, camera, lane_options(false))],
            &ledger,
            90,
        )
        .unwrap();
    assert_eq!(prior_admission.admitted_pages, 1);
    let base_budget = ledger.snapshot().unwrap().charged;
    ledger.set_budget(base_budget).unwrap();

    let (candidate, _, candidate_admission) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 2, camera, lane_options(true))],
            &ledger,
            91,
        )
        .unwrap();
    assert_eq!(candidate_admission.admitted_pages, 1);
    assert_eq!(candidate.draw_count(), 2);
    assert!(ledger.snapshot().unwrap().charged > base_budget);
    render_test_frame(&device, &queue, &renderer, &prior).unwrap();
    render_test_frame(&device, &queue, &renderer, &candidate).unwrap();
}

#[test]
fn failed_aggregate_transition_keeps_the_table_retryable_after_prior_frame_retires() {
    let Some((device, queue)) = headless_device() else {
        return;
    };

    // Size the transition reserve so the missing page and the table growth each fit alone,
    // but their aggregate does not. This exercises the reservation ordering rather than a
    // trivially undersized transition reserve.
    let probe_ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let probe_scene = test_snapshot(&probe_ledger);
    let probe_requests = unique_summary_requests(
        &[request(
            &probe_scene,
            1,
            camera(probe_scene.prepared_interval()),
            lane_options(true),
        )],
        true,
    )
    .unwrap();
    let allele = probe_requests
        .iter()
        .find(|page| page.key.family() == SummaryPageFamily::Alleles)
        .unwrap()
        .borrowed();
    let packed = PackedAlignmentSummaryPage::from_request(allele).unwrap();
    let candidate_page_bytes = packed_buffer_capacity(&packed)
        .unwrap()
        .saturating_add(packed.range_index_bytes());
    let table_transition_bytes =
        summary_page_table_retained_bytes(next_summary_capacity(2).unwrap().saturating_mul(2))
            .unwrap();
    let transition_reserve = candidate_page_bytes.max(table_transition_bytes);
    assert!(candidate_page_bytes > 0 && table_transition_bytes > 0);
    drop(probe_requests);
    drop(probe_scene);
    drop(probe_ledger);

    let ledger =
        AllocationLedger::new_with_transient_budget(64 << 20, transition_reserve, 16 << 20);
    let scene = test_snapshot(&ledger);
    let camera = camera(scene.prepared_interval());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    // A wider prior frame makes the resources released at retirement large enough for the
    // exact same candidate to succeed on retry without changing the ledger budget.
    let prior_requests = (0..8)
        .map(|sequence| request(&scene, sequence, camera, lane_options(false)))
        .collect::<Vec<_>>();
    let (prior, _, _) = renderer
        .prepare_with_admission(&device, &queue, &prior_requests, &ledger, 92)
        .unwrap();
    assert_eq!(renderer.cache.page_table_capacity, 1);
    ledger
        .set_budget(ledger.snapshot().unwrap().charged)
        .unwrap();

    let error = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 9, camera, lane_options(true))],
            &ledger,
            93,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        AlignmentSummaryGpuFrameBuildError::Admission(
            AlignmentSummaryGpuPageAdmissionError::Ledger(LedgerError::BudgetExceeded { .. })
        )
    ));
    assert_eq!(renderer.cache.page_table_capacity, 1);
    assert_eq!(renderer.resident_page_count(), 1);
    render_test_frame(&device, &queue, &renderer, &prior).unwrap();

    drop(prior);
    let (candidate, _, admission) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 10, camera, lane_options(true))],
            &ledger,
            94,
        )
        .unwrap();
    assert_eq!(admission.admitted_pages, 1);
    assert_eq!(candidate.draw_count(), 2);
    render_test_frame(&device, &queue, &renderer, &candidate).unwrap();
}

#[test]
fn idle_frame_ring_slots_are_reclaimed_after_overlapping_frames_retire() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let camera = camera(scene.prepared_interval());
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (first, _, _) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 1, camera, lane_options(false))],
            &ledger,
            100,
        )
        .unwrap();
    let (second, _, _) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 2, camera, lane_options(true))],
            &ledger,
            101,
        )
        .unwrap();
    assert_eq!(renderer.frame_buffers.len(), 2);
    drop(first);
    drop(second);

    let (_third, _, _) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(&scene, 3, camera, lane_options(false))],
            &ledger,
            102,
        )
        .unwrap();
    assert_eq!(renderer.frame_buffers.len(), 1);
}

#[test]
fn resident_byte_stats_exclude_unrelated_shared_ledger_gpu_allocations() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let unrelated_bytes = 4096;
    let _unrelated = ledger
        .reserve(
            AllocationClass::GpuPage,
            unrelated_bytes,
            ReservationKind::Normal,
        )
        .unwrap()
        .register(unrelated_bytes, 1)
        .unwrap();
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (_frame, stats, _) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(
                &scene,
                1,
                camera(scene.prepared_interval()),
                lane_options(false),
            )],
            &ledger,
            110,
        )
        .unwrap();
    assert_eq!(
        stats.resident_gpu_bytes,
        renderer.resident_gpu_bytes().unwrap()
    );
    let global_gpu_bytes = ledger
        .snapshot()
        .unwrap()
        .records
        .iter()
        .filter(|record| record.class == AllocationClass::GpuPage)
        .map(|record| record.charged_capacity)
        .sum::<usize>();
    assert_eq!(
        global_gpu_bytes,
        stats.resident_gpu_bytes.saturating_add(unrelated_bytes)
    );
}

#[test]
fn cooperative_reclaim_preserves_protected_pages_then_releases_unpinned_lru() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (frame, _, _) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(
                &scene,
                1,
                camera(scene.prepared_interval()),
                lane_options(false),
            )],
            &ledger,
            120,
        )
        .unwrap();
    drop(frame);
    let key = renderer.cache.pages[0].key;
    let snapshot = ledger.snapshot().unwrap();
    let free = snapshot
        .budget
        .saturating_sub(snapshot.charged.saturating_add(snapshot.reserved));
    let required = free.saturating_add(renderer.resident_retained_bytes().unwrap());

    let protected = renderer
        .reclaim_for_headroom(&ledger, required, &[key])
        .unwrap();
    assert_eq!(protected.reclaimed_pages(), 0);
    assert_eq!(renderer.resident_page_count(), 1);
    assert!(!protected.headroom_satisfied);

    let reclaimed = renderer
        .reclaim_for_headroom(&ledger, required, &[])
        .unwrap();
    assert_eq!(reclaimed.reclaimed_pages(), 1);
    assert_eq!(renderer.resident_page_count(), 0);
    assert!(reclaimed.released_retained_bytes() > 0);
    assert!(reclaimed.released_ledger_bytes() > 0);
    assert!(reclaimed.headroom_satisfied);
}

#[test]
fn cooperative_reclaim_batches_many_summary_pages_around_protected_and_pinned_members() {
    let Some((device, _queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let page_bytes = 256;
    // The first six canonical families retain the original protected/pinned/LRU ordering.
    // The synthetic companion key is deliberately newest, so adding a supported semantic
    // family does not silently turn it into one of the two pages this fixture asks reclaim
    // to choose.
    let revisions = [60, 10, 50, 20, 40, 30, 70];
    let mut keys = scene
        .summary_pages()
        .iter()
        .map(|page| AlignmentSummaryGpuPageKey {
            semantic: page.key(),
            companion_depth: None,
        })
        .collect::<Vec<_>>();
    let extra_semantic = keys[0].semantic;
    let extra_companion = keys[1].semantic;
    keys.push(AlignmentSummaryGpuPageKey {
        semantic: extra_semantic,
        companion_depth: Some(extra_companion),
    });
    keys.sort_unstable();
    assert_eq!(keys.len(), revisions.len());
    for (key, revision) in keys.iter().copied().zip(revisions) {
        let allocation = ledger
            .reserve(
                AllocationClass::GpuPage,
                page_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(page_bytes, revision)
            .unwrap();
        renderer.cache.pages.push(GpuSummaryPage {
            key,
            interval_start: 0,
            buffer: None,
            instance_count: 0,
            metadata: GpuSummaryPageMetadata::Alleles,
            allocation: Some(allocation),
            last_used_revision: revision,
        });
    }
    renderer.cache.pages.sort_unstable_by_key(|page| page.key);
    renderer.rebuild_reclaim_index();

    let pinned_key = keys[3];
    let pin = renderer
        .page(pinned_key)
        .unwrap()
        .allocation
        .as_ref()
        .unwrap()
        .pin()
        .unwrap();
    let protected = [keys[5], keys[1], keys[5]];
    let pages_before = renderer
        .cache
        .pages
        .iter()
        .map(|page| page.key)
        .collect::<Vec<_>>();
    let ledger_before = ledger.snapshot().unwrap();
    let infeasible_pressure = GpuBudgetPressure::from_budget_failure(
        ledger_before.budget.saturating_add(1),
        ledger_before.charged,
        ledger_before.reserved,
        ledger_before.budget,
    );
    let infeasible = renderer
        .reclaim_for_budget_pressure(&ledger, infeasible_pressure, &protected)
        .unwrap();
    assert_eq!(infeasible.reclaimed_pages(), 0);
    assert!(!infeasible.headroom_satisfied);
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>(),
        pages_before
    );

    let eligible_lru = [keys[4], keys[2]];
    let before = ledger.snapshot().unwrap();
    let pressure = GpuBudgetPressure::from_budget_failure(
        page_bytes.saturating_mul(eligible_lru.len()),
        before.charged,
        before.reserved,
        before.charged.saturating_add(before.reserved),
    );
    let operations_before = renderer.operation_counters();
    let report = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &protected)
        .unwrap();
    let operations_after = renderer.operation_counters();
    let remaining = renderer
        .cache
        .pages
        .iter()
        .map(|page| page.key)
        .collect::<Vec<_>>();

    assert_eq!(report.reclaimed_pages(), eligible_lru.len());
    assert!(report.headroom_satisfied);
    assert!(eligible_lru.iter().all(|key| !remaining.contains(key)));
    assert!(remaining.contains(&keys[0]));
    assert!(protected.iter().all(|key| remaining.contains(key)));
    assert!(remaining.contains(&pinned_key));
    assert!(remaining.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        operations_after.reclaim_authority_keys_inspected
            - operations_before.reclaim_authority_keys_inspected,
        5,
        "the online index stops after protected, pinned, and sufficient LRU candidates"
    );
    assert!(
        operations_after.reclaim_authority_keys_inspected
            - operations_before.reclaim_authority_keys_inspected
            < u64::try_from(pages_before.len()).unwrap()
    );
    drop(pin);
}

#[test]
fn exact_transition_pressure_does_not_reclaim_when_the_original_limit_already_fits() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let transition_reserve = 20;
    let ledger =
        AllocationLedger::new_with_transient_budget(64 << 20, transition_reserve, 16 << 20);
    let scene = test_snapshot(&ledger);
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let frame = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(
                &scene,
                1,
                camera(scene.prepared_interval()),
                lane_options(false),
            )],
            &ledger,
            120,
        )
        .unwrap()
        .0;
    drop(frame);
    let before = ledger.snapshot().unwrap();
    let base_budget = before.charged.saturating_sub(15);
    ledger.set_budget(base_budget).unwrap();
    let pressure = GpuBudgetPressure::from_budget_failure(
        5,
        before.charged,
        before.reserved,
        base_budget.saturating_add(transition_reserve),
    );
    let pages_before = renderer.resident_page_count();
    let retained_before = renderer.resident_retained_bytes().unwrap();
    let report = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();

    assert!(report.headroom_satisfied);
    assert_eq!(report.pressure, pressure);
    assert_eq!(report.reclaimed_pages(), 0);
    assert_eq!(report.released_retained_bytes(), 0);
    assert_eq!(renderer.resident_page_count(), pages_before);
    assert_eq!(renderer.resident_retained_bytes().unwrap(), retained_before);
    assert!(report.ledger_charged_before > base_budget);
    assert_eq!(report.ledger_charged_after, report.ledger_charged_before);
}

#[test]
fn bisulfite_interpretation_matches_the_cpu_oracle_and_reuses_allele_pages() {
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let mut reference = vec![b'A'; 100];
    reference[11] = b'C';
    reference[12] = b'G';
    let scene = test_snapshot_with_reference(&ledger, reference);
    let mut options = lane_options(true);
    let mut alleles = options.alleles.unwrap();
    alleles.interpretation = CoverageAlleleInterpretation::Bisulfite(BisulfiteContext::Cg);
    alleles.quality_weighted = false;
    alleles.minimum_fraction = 0.99;
    options.alleles = Some(alleles);
    let camera = camera(scene.prepared_interval());
    let cpu = CpuSummaryProjector::project(
        &scene,
        1,
        camera,
        &CpuSummaryProjectionOptions::Alleles(alleles),
    )
    .unwrap();
    let allele_page = scene
        .summary_pages()
        .iter()
        .find(|page| page.key().family() == SummaryPageFamily::Alleles)
        .unwrap();
    let depth_page = scene
        .summary_pages()
        .iter()
        .find(|page| page.key().family() == SummaryPageFamily::Depth)
        .unwrap();
    let PackedAlignmentSummaryPage::Alleles { instances } =
        PackedAlignmentSummaryPage::from_request(AlignmentSummaryGpuPageRequest::alleles(
            allele_page,
            depth_page,
        ))
        .unwrap()
    else {
        unreachable!();
    };
    let mut gpu = instances
        .iter()
        .filter_map(|instance| {
            project_alignment_summary_bisulfite_instance(
                *instance,
                allele_page.interval().start(),
                camera,
                1,
                CoverageScaleTransform::Linear,
                false,
                BisulfiteContext::Cg,
            )
        })
        .collect::<Vec<_>>();
    let mut cpu = cpu
        .primitives
        .iter()
        .filter_map(|primitive| {
            let CpuSummaryProjectedComponent::Bisulfite {
                observation,
                strand,
                fraction_bits,
                ..
            } = primitive.component
            else {
                return None;
            };
            Some((
                primitive,
                observation,
                strand,
                f64::from_bits(fraction_bits),
            ))
        })
        .collect::<Vec<_>>();
    gpu.sort_by(|left, right| {
        left.bounds
            .x
            .total_cmp(&right.bounds.x)
            .then_with(|| left.bounds.y.total_cmp(&right.bounds.y))
    });
    cpu.sort_by(|left, right| {
        left.0
            .x
            .total_cmp(&right.0.x)
            .then_with(|| left.0.y.total_cmp(&right.0.y))
    });
    assert!(!gpu.is_empty());
    assert_eq!(gpu.len(), cpu.len());
    for (gpu, (cpu, observation, strand, fraction)) in gpu.iter().zip(cpu) {
        assert_eq!(gpu.observation, observation);
        assert_eq!(gpu.strand, strand);
        assert_near(gpu.fraction, fraction);
        assert_near(gpu.bounds.x, cpu.x);
        assert_near(gpu.bounds.y, cpu.y);
        assert_near(gpu.bounds.width, cpu.width);
        assert_near(gpu.bounds.height, cpu.height);
    }

    let Some((device, queue)) = headless_device() else {
        return;
    };
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let generic = [request(&scene, 1, camera, lane_options(true))];
    let (first, _, _) = publish_bundled_summary_frame(
        &device,
        &queue,
        &mut renderer,
        &generic,
        &ledger,
        Vec::new(),
        1,
        500,
    );
    let authority = first.content_authority().clone();
    let (bisulfite, stats) = renderer
        .capture_camera_frame(
            &device,
            &queue,
            &authority,
            &[request(&scene, 2, camera, options)],
            &ledger,
        )
        .unwrap();
    assert_eq!(stats.geometry_uploaded_bytes, 0);
    render_test_frame(&device, &queue, &renderer, &bisulfite).unwrap();
}

#[test]
fn only_persistent_budget_failures_request_cross_cache_reclaim() {
    let budget = LedgerError::BudgetExceeded {
        requested: 4_096,
        charged: 8_192,
        reserved: 0,
        limit: 10_000,
    };
    let admission = AlignmentSummaryGpuFrameBuildError::Admission(
        AlignmentSummaryGpuPageAdmissionError::Ledger(budget),
    );
    assert_eq!(
        admission.persistent_budget_pressure(),
        Some(GpuBudgetPressure::from_budget_failure(
            4_096, 8_192, 0, 10_000,
        ))
    );

    let nested =
        AlignmentSummaryGpuFrameBuildError::Prepare(AlignmentSummaryGpuPrepareError::Admission(
            AlignmentSummaryGpuPageAdmissionError::Ledger(budget),
        ));
    assert_eq!(
        nested.persistent_budget_pressure(),
        Some(GpuBudgetPressure::from_budget_failure(
            4_096, 8_192, 0, 10_000,
        ))
    );

    let transient = AlignmentSummaryGpuFrameBuildError::Prepare(
        AlignmentSummaryGpuPrepareError::Ledger(LedgerError::TransientBudgetExceeded {
            class: TransientResourceClass::GpuUpload,
            requested: 4_096,
            transient: 8_192,
            limit: 10_000,
        }),
    );
    assert_eq!(transient.persistent_budget_pressure(), None);
    assert_eq!(
        AlignmentSummaryGpuFrameBuildError::Prepare(
            AlignmentSummaryGpuPrepareError::InvalidFrameGeometry,
        )
        .persistent_budget_pressure(),
        None
    );
}

#[test]
fn coverage_frame_remains_available_when_the_reads_lane_is_collapsed() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_snapshot(&ledger);
    let mut renderer =
        AlignmentSummaryGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    // This request deliberately has no read camera, row range, or read-renderer dependency.
    let (frame, stats, admission) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            &[request(
                &scene,
                1,
                camera(scene.prepared_interval()),
                lane_options(false),
            )],
            &ledger,
            80,
        )
        .unwrap();
    assert_eq!(admission.admitted_pages, 1);
    assert_eq!(stats.mandatory_pages, 1);
    assert!(stats.depth_instances > 0);
    assert_eq!(frame.draw_count(), 1);
    render_test_frame(&device, &queue, &renderer, &frame).unwrap();
}

fn render_test_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &AlignmentSummaryGpuRenderer,
    frame: &AlignmentSummaryGpuFrame,
) -> Result<(), AlignmentSummaryGpuRenderError> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("RGV alignment summary test target"),
        size: wgpu::Extent3d {
            width: 256,
            height: 96,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("RGV alignment summary test encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("RGV alignment summary test pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer.render(&mut pass, frame)?;
    }
    queue.submit([encoder.finish()]);
    Ok(())
}
