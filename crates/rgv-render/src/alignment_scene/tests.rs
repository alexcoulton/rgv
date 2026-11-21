use std::{
    num::{NonZeroU32, NonZeroU64},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentHandle, AlignmentOperation, AlignmentPageKey, AlignmentPresentationScene,
    AlignmentReadLabelMode, AlignmentRowAtlas, AlignmentSemanticPage, AlignmentSourceRevision,
    AllocationBundleSpec, AllocationClass, AllocationLedger, CanonicalGenomicTile,
    CanonicalRecordChunk, CanonicalRowBlock, CapabilityRequirement, CompleteCoverage,
    ContractFingerprint, EvidenceAcquisition, EvidenceChunk, EvidenceRevision, EvidenceRootBuilder,
    FilteredRecordProvenance, GeometryLod, LayoutContract, PayloadContract, PayloadSidecar,
    PresentationContract, ReservationKind, RevisionFingerprint, RowAtlasRevision, SceneRevision,
    SourceChunkId, SourceFingerprint,
};
use rgv_core::{
    AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId, GenomicInterval,
    ReadStackingMode,
};
use rgv_hts::{
    AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
    AlignmentSourcePosition, CigarOp, CigarOpKind, SequenceData, TemplateLengthBounds,
};

use super::*;

fn handle() -> AlignmentHandle {
    AlignmentHandle::new(
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        ),
        AlignmentSourcePosition::bam_virtual_offset(5),
    )
}

fn primitive(component: AlignmentComponent) -> AlignmentSemanticPrimitive {
    let geometry = match component {
        AlignmentComponent::Deletion { .. }
        | AlignmentComponent::ReferenceSkip { .. }
        | AlignmentComponent::PairConnector { .. } => AlignmentGeometryToken::EventLine,
        AlignmentComponent::Insertion { .. }
        | AlignmentComponent::SoftClip { .. }
        | AlignmentComponent::HardClip { .. }
        | AlignmentComponent::Padding { .. }
        | AlignmentComponent::UltimaIndel { .. }
        | AlignmentComponent::SbxInsertion { .. } => AlignmentGeometryToken::PointMarker,
        AlignmentComponent::DirectionTerminal => AlignmentGeometryToken::DirectionalTerminalRight,
        _ => AlignmentGeometryToken::Rectangle,
    };
    AlignmentSemanticPrimitive {
        start_offset: 16_777_217,
        end_offset: 16_777_219,
        row_offset: 7,
        handle: handle(),
        component,
        geometry,
        style: AlignmentStyleToken {
            flags: 0,
            mapping_quality: Some(60),
            operation: Some(AlignmentOperation::SequenceMatch),
            body: AlignmentBodyColorToken::Category(Some(ContractFingerprint::new(99))),
        },
        draw_order: 3,
    }
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
        label: Some("RGV alignment scene test device"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
    }))
    .ok()
}

fn render_test_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &AlignmentGpuRenderer,
    frame: &AlignmentGpuFrame,
) -> Result<(), AlignmentGpuRenderError> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("RGV alignment scene test target"),
        size: wgpu::Extent3d {
            width: 256,
            height: 128,
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
        label: Some("RGV alignment scene test encoder"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("RGV alignment scene test pass"),
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

fn publish_bundled_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut AlignmentGpuRenderer,
    requests: &[AlignmentGpuSceneRequest<'_>],
    ledger: &AllocationLedger,
    mut prior_frame: Option<AlignmentGpuFrame>,
    first_slot: u32,
    publication_revision: u64,
) -> (
    AlignmentGpuFrame,
    AlignmentGpuPrepareStats,
    AlignmentGpuBatchAdmission,
) {
    let snapshot = ledger.snapshot().unwrap();
    let plan = renderer
        .plan_replacement(requests, &snapshot, AllocationBundleSlot::new(first_slot))
        .unwrap();
    let retirement = renderer
        .detach_replacement_retirement(ledger, &plan, &mut prior_frame)
        .unwrap();
    assert!(prior_frame.is_none());
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

fn test_scene(ledger: &AllocationLedger) -> Arc<AlignmentPresentationScene> {
    test_scene_with_page_shape(
        ledger,
        NonZeroU64::new(100).unwrap(),
        1,
        NonZeroU32::new(16).unwrap(),
    )
}

fn test_scene_with_tile_bases(
    ledger: &AllocationLedger,
    tile_bases: NonZeroU64,
) -> Arc<AlignmentPresentationScene> {
    test_scene_with_page_shape(ledger, tile_bases, 1, NonZeroU32::new(16).unwrap())
}

fn test_scene_with_tiled_records(
    ledger: &AllocationLedger,
    tile_bases: NonZeroU64,
) -> Arc<AlignmentPresentationScene> {
    let mut starts = Vec::new();
    let mut start = 5_u64;
    while start.saturating_add(10) <= 100 {
        starts.push(start);
        start = start.saturating_add(tile_bases.get());
    }
    test_scene_with_record_starts(ledger, tile_bases, &starts, NonZeroU32::new(16).unwrap())
}

fn test_scene_with_page_shape(
    ledger: &AllocationLedger,
    tile_bases: NonZeroU64,
    overlapping_records: usize,
    row_block_rows: NonZeroU32,
) -> Arc<AlignmentPresentationScene> {
    test_scene_with_record_starts(
        ledger,
        tile_bases,
        &vec![10; overlapping_records],
        row_block_rows,
    )
}

fn test_scene_with_record_starts(
    ledger: &AllocationLedger,
    tile_bases: NonZeroU64,
    record_starts: &[u64],
    row_block_rows: NonZeroU32,
) -> Arc<AlignmentPresentationScene> {
    let reads = record_starts
        .iter()
        .map(|start| TestRead::forward(*start))
        .collect::<Vec<_>>();
    test_scene_over(
        ledger,
        GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
        100,
        tile_bases,
        &reads,
        row_block_rows,
    )
}

/// One ten-base fixture read. `cigar` must consume exactly ten query bases.
#[derive(Clone, Debug)]
pub(crate) struct TestRead {
    pub start: u64,
    pub reverse: bool,
    pub cigar: Vec<CigarOp>,
}

impl TestRead {
    pub(crate) fn forward(start: u64) -> Self {
        Self {
            start,
            reverse: false,
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 10,
            }],
        }
    }
}

/// Builds the fixture scene over an arbitrary prepared `interval` of a contig of
/// `contig_length` bases, so pages can start far from the reads they carry.
pub(crate) fn test_scene_over(
    ledger: &AllocationLedger,
    interval: GenomicInterval,
    contig_length: u64,
    tile_bases: NonZeroU64,
    reads: &[TestRead],
    row_block_rows: NonZeroU32,
) -> Arc<AlignmentPresentationScene> {
    let source_revision = AlignmentSourceRevision::new(
        SourceFingerprint::new(RevisionFingerprint::new(11, 12)),
        RevisionFingerprint::new(13, 14),
    );
    let record_contract = ContractFingerprint::new(15);
    let record = AlignmentRecord {
        contig: ContigId::new(0),
        start: 10,
        end: 20,
        flags: AlignmentFlags::from_bits(0),
        mapping_quality: Some(60),
        mate_contig: None,
        mate_start: None,
        template_length: 0,
        read_name: b"gpu".to_vec(),
        cigar: vec![CigarOp {
            kind: CigarOpKind::Match,
            len: 10,
        }],
        sequence: SequenceData::Present {
            bases: b"ACGTACGTAC".to_vec(),
            qualities: Some(vec![35; 10]),
        },
        auxiliary_fields: Vec::new(),
    };
    let mut decoded = AlignmentBatch::default();
    for (ordinal, read) in reads.iter().enumerate() {
        let mut record = record.clone();
        record.start = read.start;
        let reference_bases = read
            .cigar
            .iter()
            .filter(|operation| {
                matches!(
                    operation.kind,
                    CigarOpKind::Match
                        | CigarOpKind::Deletion
                        | CigarOpKind::ReferenceSkip
                        | CigarOpKind::SequenceMatch
                        | CigarOpKind::SequenceMismatch
                )
            })
            .map(|operation| u64::from(operation.len))
            .sum::<u64>();
        record.end = read.start.saturating_add(reference_bases);
        record.cigar = read.cigar.clone();
        if read.reverse {
            record.flags = AlignmentFlags::from_bits(0x10);
        }
        decoded
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset(
                    (1 << 16) | u64::try_from(ordinal).unwrap(),
                ),
            )
            .unwrap();
    }
    let payload_contract = PayloadContract::new_all_auxiliary(
        CapabilityRequirement::Required,
        CapabilityRequirement::Required,
        None,
    );
    let payload = Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
    let core = CanonicalRecordChunk::new(
        source_revision,
        SourceChunkId::BamBlock {
            compressed_offset: 1,
        },
        Arc::new(decoded.copy_core().unwrap()),
        record_contract,
    )
    .unwrap();
    let payload_bytes = payload.estimated_bytes();
    let mut payload_reservation = ledger
        .reserve(
            AllocationClass::PayloadSidecar,
            payload_bytes,
            ReservationKind::Normal,
        )
        .unwrap();
    let payload_allocation = payload_reservation.register(payload_bytes, 1).unwrap();
    let payload = Arc::new(
        PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation).unwrap(),
    );
    let footprint = EvidenceChunk::footprint(&core);
    let mut core_reservation = ledger
        .reserve(
            AllocationClass::CoreChunk,
            footprint.core_bytes,
            ReservationKind::Normal,
        )
        .unwrap();
    let core_allocation = core_reservation.register(footprint.core_bytes, 1).unwrap();
    let mut overlap_reservation = ledger
        .reserve(
            AllocationClass::OverlapIndex,
            footprint.overlap_index_bytes,
            ReservationKind::Normal,
        )
        .unwrap();
    let overlap_allocation = overlap_reservation
        .register(footprint.overlap_index_bytes, 1)
        .unwrap();
    let chunk = Arc::new(
        EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
            .unwrap(),
    );
    let coverage = CompleteCoverage::new(
        source_revision,
        record_contract,
        interval,
        FilteredRecordProvenance {
            records_seen: u64::try_from(reads.len()).unwrap(),
            eligible_records: u64::try_from(reads.len()).unwrap(),
            ..FilteredRecordProvenance::default()
        },
    )
    .unwrap();
    let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
    let mut evidence_builder = EvidenceRootBuilder::new(
        EvidenceRevision::new(RevisionFingerprint::new(16, 17)),
        source_revision,
        record_contract,
        payload_contract,
        1,
        1,
        ledger,
        ReservationKind::Normal,
    )
    .unwrap();
    evidence_builder.insert_acquisition(&acquisition).unwrap();
    let evidence = evidence_builder.build(2).unwrap();
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
        RowAtlasRevision::new(RevisionFingerprint::new(18, 19)),
        &evidence,
        interval,
        layout,
        ledger,
        ReservationKind::Normal,
        3,
    )
    .unwrap();
    let presentation = PresentationContract {
        geometry_lod: GeometryLod::Read,
        style: rgv_alignment_scene::AlignmentStyleContract {
            read_labels: AlignmentReadLabelMode::ReadName,
            ..rgv_alignment_scene::AlignmentStyleContract::default()
        },
        reference: None,
        annotation: None,
    };
    let rows = 0..u32::try_from(atlas.row_count()).unwrap();
    let mut pages = Vec::new();
    for tile in CanonicalGenomicTile::covering(interval, tile_bases, contig_length).unwrap() {
        let row_blocks = rows
            .end
            .saturating_add(row_block_rows.get().saturating_sub(1))
            / row_block_rows.get();
        for row_block in 0..row_blocks {
            for family in presentation.required_page_families() {
                let key = AlignmentPageKey::new(
                    source_revision,
                    evidence.revision(),
                    atlas.revision(),
                    layout.fingerprint(),
                    presentation.page_fingerprint(family),
                    tile,
                    CanonicalRowBlock::new(row_block, row_block_rows),
                    family,
                    GeometryLod::Read,
                );
                let page = AlignmentSemanticPage::build(
                    key,
                    &evidence,
                    &atlas,
                    presentation,
                    contig_length,
                    None,
                    ledger,
                    ReservationKind::Normal,
                    4,
                )
                .unwrap();
                pages.push(page);
            }
        }
    }
    AlignmentPresentationScene::new(
        SceneRevision::new(RevisionFingerprint::new(20, 21)),
        evidence,
        atlas,
        presentation,
        interval,
        rows,
        pages,
        ledger,
        ReservationKind::Normal,
        5,
    )
    .unwrap()
}

#[test]
fn page_local_coordinates_survive_beyond_single_f32_integer_precision() {
    let instance = pack_primitive(primitive(AlignmentComponent::ReadSpan))
        .unwrap()
        .unwrap();
    assert_eq!(
        f64::from(instance.start_offset[0]) + f64::from(instance.start_offset[1]),
        16_777_217.0
    );
    assert_eq!(
        f64::from(instance.end_offset[0]) + f64::from(instance.end_offset[1]),
        16_777_219.0
    );
}

#[test]
fn semantic_component_families_map_to_stable_gpu_shapes_without_labels() {
    assert_eq!(
        pack_primitive(primitive(AlignmentComponent::Deletion { cigar_index: 1 }))
            .unwrap()
            .unwrap()
            .shape,
        AlignmentGpuShape::EventLine as u32
    );
    assert_eq!(
        pack_primitive(primitive(AlignmentComponent::Insertion {
            cigar_index: 1,
            read_start: 2,
            length: 3,
        }))
        .unwrap()
        .unwrap()
        .shape,
        AlignmentGpuShape::PointMarker as u32
    );
    assert_eq!(
        pack_primitive(primitive(AlignmentComponent::DirectionTerminal))
            .unwrap()
            .unwrap()
            .shape,
        AlignmentGpuShape::DirectionTerminalRight as u32
    );
    assert!(
        pack_primitive(primitive(AlignmentComponent::Label {
            kind: rgv_alignment_scene::AlignmentLabelKind::ReadName,
            id: ContractFingerprint::new(7),
        }))
        .unwrap()
        .is_none()
    );
}

#[test]
fn stable_handle_interaction_is_separate_from_immutable_base_geometry() {
    let semantic = primitive(AlignmentComponent::ReadSpan);
    let base = pack_primitive(semantic).unwrap().unwrap();
    let interaction = AlignmentInteractionState::new(Some(handle()), Some(handle()), []);
    let token = interaction.token(handle());
    let flags = interaction_flags(token);

    assert_eq!(base, pack_primitive(semantic).unwrap().unwrap());
    assert_ne!(flags, 0);
    assert_ne!(base.color, reference_interaction_color(base.color, token));
}

#[test]
fn neutral_hover_selection_and_highlight_have_distinct_read_colours() {
    let mut semantic = primitive(AlignmentComponent::ReadSpan);
    semantic.style.body = AlignmentBodyColorToken::Neutral;
    let neutral = style_color(semantic.style);
    assert_eq!(neutral, [0.73, 0.73, 0.73, 1.0]);
    let hovered = reference_interaction_color(
        neutral,
        rgv_alignment_scene::AlignmentInteractionToken {
            hovered: true,
            ..rgv_alignment_scene::AlignmentInteractionToken::default()
        },
    );
    let selected = reference_interaction_color(
        neutral,
        rgv_alignment_scene::AlignmentInteractionToken {
            selected: true,
            ..rgv_alignment_scene::AlignmentInteractionToken::default()
        },
    );
    let highlighted = reference_interaction_color(
        neutral,
        rgv_alignment_scene::AlignmentInteractionToken {
            highlighted: true,
            ..rgv_alignment_scene::AlignmentInteractionToken::default()
        },
    );
    assert_ne!(neutral, hovered);
    assert_ne!(hovered, selected);
    assert_ne!(selected, highlighted);
    assert!(selected[2] > selected[0]);
    assert!(highlighted[0] > highlighted[2]);
}

#[test]
fn camera_uniform_is_host_shareable_and_double_single() {
    assert_eq!(size_of::<AlignmentGpuCameraUniform>(), 48);
    let split = split_f64(5_525_127.3);
    assert!(((f64::from(split[0]) + f64::from(split[1])) - 5_525_127.3).abs() < 1.0e-7);
}

#[test]
fn packed_gpu_geometry_matches_the_cpu_reference_projector() {
    let ledger = AllocationLedger::new(16 << 20, 2 << 20);
    let scene = test_scene(&ledger);
    let camera = AlignmentCamera::new(
        scene.prepared_interval(),
        0.0,
        12.0,
        9.0,
        rgv_alignment_scene::CpuClipRect {
            x: 17.0,
            y: 23.0,
            width: 503.0,
            height: 137.0,
        },
    )
    .unwrap();
    assert_gpu_oracle_matches_cpu_projector(&scene, camera, false);
}

#[test]
fn sub_pixel_gpu_geometry_keeps_one_point_and_matches_the_cpu_projector() {
    let ledger = AllocationLedger::new(16 << 20, 2 << 20);
    let scene = test_scene(&ledger);
    // The whole prepared interval squeezed into three points, with rows a quarter of a
    // point tall: every primitive is far below one point in both axes.
    let camera = AlignmentCamera::new(
        scene.prepared_interval(),
        0.0,
        0.25,
        0.2,
        rgv_alignment_scene::CpuClipRect {
            x: 17.0,
            y: 23.0,
            width: 3.0,
            height: 137.0,
        },
    )
    .unwrap();
    assert_gpu_oracle_matches_cpu_projector(&scene, camera, true);
}

#[test]
fn immutable_page_buffers_are_exactly_sized() {
    let stride = size_of::<AlignmentGpuInstance>();
    assert_eq!(stride % wgpu::COPY_BUFFER_ALIGNMENT as usize, 0);
    assert_eq!(gpu_buffer_capacity::<AlignmentGpuInstance>(0).unwrap(), 0);
    for count in [1_usize, 2, 3, 5, 1_000, 1_025, 65_537] {
        // Exactly the uploaded bytes: no power-of-two slack to waste or to mis-account.
        assert_eq!(
            gpu_buffer_capacity::<AlignmentGpuInstance>(count).unwrap(),
            count * stride
        );
    }
    // Odd strides are rounded up only to wgpu's copy alignment.
    assert_eq!(exact_immutable_buffer_bytes(3, 5), Some(16));
    assert_eq!(exact_immutable_buffer_bytes(4, 5), Some(20));
    assert_eq!(exact_immutable_buffer_bytes(usize::MAX, 2), None);
    assert!(matches!(
        gpu_buffer_capacity::<AlignmentGpuInstance>(usize::MAX),
        Err(AlignmentGpuPageAdmissionError::CapacityOverflow)
    ));
}

#[test]
fn rebuilding_for_a_replacement_device_drops_pages_and_advances_the_generation() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene(&ledger);
    let baseline = ledger.snapshot().unwrap().charged;
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Bgra8Unorm);
    renderer
        .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
        .unwrap();
    assert!(renderer.resident_page_count() > 0);
    assert!(ledger.snapshot().unwrap().charged > baseline);
    let generation = renderer.device_generation;

    // Stand-in for a lost device: a second, independent device.
    let Some((replacement, replacement_queue)) = headless_device() else {
        return;
    };
    renderer.rebuild_for_device(&replacement, wgpu::TextureFormat::Rgba8Unorm);
    assert_eq!(renderer.resident_page_count(), 0);
    assert!(renderer.device_generation > generation);
    assert_eq!(ledger.snapshot().unwrap().charged, baseline);

    // The rebuilt renderer is fully usable on the replacement device.
    renderer
        .admit_pages(&replacement, &replacement_queue, scene.pages(), &ledger, 11)
        .unwrap();
    assert!(renderer.resident_page_count() > 0);
}

fn assert_gpu_oracle_matches_cpu_projector(
    scene: &Arc<AlignmentPresentationScene>,
    camera: AlignmentCamera,
    expect_minimum_size: bool,
) {
    let cpu = rgv_alignment_scene::CpuAlignmentProjector::project(scene, 1, camera).unwrap();
    let mut compared = 0_usize;

    for page in scene.pages() {
        let packed = PackedAlignmentPage::from_semantic(page).unwrap();
        for (instance, semantic_index) in packed
            .instances
            .iter()
            .copied()
            .zip(packed.semantic_indices.iter().copied())
        {
            let semantic = page.primitives()[semantic_index as usize];
            let Some(gpu) = project_alignment_gpu_instance(
                instance,
                page.interval().start(),
                page.row_range().start,
                camera,
            ) else {
                continue;
            };
            compared += 1;
            if expect_minimum_size {
                // Unless the pane edge cuts it, nothing is thinner than one point.
                let clip = camera.clip();
                let cut_horizontally =
                    gpu.x <= clip.x + 1.0e-9 || gpu.x + gpu.width >= clip.x + clip.width - 1.0e-9;
                let cut_vertically =
                    gpu.y <= clip.y + 1.0e-9 || gpu.y + gpu.height >= clip.y + clip.height - 1.0e-9;
                assert!(cut_horizontally || gpu.width >= 1.0 - 1.0e-9, "{gpu:?}");
                assert!(cut_vertically || gpu.height >= 1.0 - 1.0e-9, "{gpu:?}");
            }
            let row = page.row_range().start + semantic.row_offset;
            let matching = cpu.primitives.iter().find(|candidate| {
                candidate.handle == semantic.handle
                    && candidate.component == semantic.component
                    && candidate.row == row
                    && (candidate.x - gpu.x).abs() < 1.0e-6
                    && (candidate.y - gpu.y).abs() < 1.0e-6
                    && (candidate.width - gpu.width).abs() < 1.0e-6
                    && (candidate.height - gpu.height).abs() < 1.0e-6
            });
            assert!(
                matching.is_some(),
                "GPU instance disagrees with CPU component {:?}",
                semantic.component
            );
        }
    }
    assert!(
        compared > 0,
        "the camera must project at least one instance"
    );
}

#[test]
fn dedicated_alignment_pipeline_is_valid_on_an_available_headless_adapter() {
    let Some((device, _queue)) = headless_device() else {
        return;
    };

    let renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    assert_eq!(renderer.resident_page_count(), 0);
}

#[test]
fn mandatory_batch_failure_never_publishes_a_partial_gpu_page_set() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene(&ledger);
    let baseline = ledger.snapshot().unwrap().charged;
    ledger.set_budget(baseline.saturating_add(1)).unwrap();
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let result = renderer.admit_pages(&device, &queue, scene.pages(), &ledger, 10);
    assert!(matches!(
        result,
        Err(AlignmentGpuPageAdmissionError::Ledger(
            LedgerError::BudgetExceeded { .. }
        ))
    ));
    assert_eq!(renderer.resident_page_count(), 0);

    ledger.set_budget(16 << 20).unwrap();
    let admitted = renderer
        .admit_pages(&device, &queue, scene.pages(), &ledger, 11)
        .unwrap();
    assert_eq!(admitted.requested_pages, scene.drawable_page_keys().len());
    assert_eq!(admitted.admitted_pages, scene.drawable_page_keys().len());
    assert_eq!(
        renderer.resident_page_count(),
        scene.drawable_page_keys().len()
    );
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
        alignment_page_table_retained_bytes(renderer.cache.page_table_capacity).unwrap()
    );
}

#[test]
fn label_only_pages_remain_cpu_owned_across_replacement_capture_and_reclaim() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene(&ledger);
    let label_page = scene
        .pages()
        .iter()
        .find(|page| {
            !page.primitives().is_empty()
                && page.primitives().iter().all(|primitive| {
                    matches!(primitive.component, AlignmentComponent::Label { .. })
                })
        })
        .expect("the labelled-read fixture contains one CPU-only label page");
    let non_drawable_keys = scene
        .pages()
        .iter()
        .filter(|page| !alignment_page_has_gpu_geometry(page))
        .map(|page| page.key())
        .collect::<BTreeSet<_>>();
    assert!(non_drawable_keys.contains(&label_page.key()));
    assert!(!scene.drawable_page_keys().is_empty());

    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    assert_eq!(
        renderer
            .admit_page(&device, &queue, label_page, &ledger, 9)
            .unwrap(),
        AlignmentGpuPageAdmission::NoGpuGeometry
    );
    assert_eq!(renderer.resident_page_count(), 0);

    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 256,
        height: 128,
    };
    let first_requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    }];
    let (first, _, _) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        None,
        20,
        30,
    );
    assert_eq!(
        first.content_authority().page_manifest(),
        scene.drawable_page_keys().as_ref()
    );
    assert!(
        first
            .draws
            .iter()
            .all(|draw| !non_drawable_keys.contains(&draw.page))
    );
    assert!(
        renderer
            .cache
            .pages
            .iter()
            .all(|page| !non_drawable_keys.contains(&page.key))
    );
    assert!(
        first
            .label_frames()
            .iter()
            .any(|frame| !frame.labels.is_empty())
    );

    let authority = first.content_authority().clone();
    let second_requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 2,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    }];
    let (second, stats) = renderer
        .capture_camera_frame(&device, &queue, &authority, &second_requests, &ledger)
        .unwrap();
    assert_eq!(stats.geometry_uploaded_bytes, 0);
    assert!(
        second
            .label_frames()
            .iter()
            .any(|frame| !frame.labels.is_empty())
    );

    let resident = renderer.resident_page_count();
    let snapshot = ledger.snapshot().unwrap();
    let pressure = GpuBudgetPressure::from_budget_failure(
        renderer.resident_retained_bytes().unwrap(),
        snapshot.charged,
        snapshot.reserved,
        snapshot.charged.saturating_add(snapshot.reserved),
    );
    let pinned = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();
    assert_eq!(pinned.reclaimed_pages(), 0);
    assert_eq!(renderer.resident_page_count(), resident);
    render_test_frame(&device, &queue, &renderer, &second).unwrap();

    drop(second);
    drop(first);
    drop(authority);
    let released = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();
    assert_eq!(released.reclaimed_pages(), resident);
    assert_eq!(renderer.resident_page_count(), 0);
}

#[test]
fn frame_boundary_admission_uploads_once_then_pan_changes_only_camera_state() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene(&ledger);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 256,
        height: 128,
    };
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    };
    let (first, first_stats, first_admission) = renderer
        .prepare_with_admission(&device, &queue, &[request], &ledger, 10)
        .unwrap();
    assert!(first_admission.admitted_pages > 0);
    assert!(first_stats.geometry_uploaded_bytes > 0);
    drop(first);

    let viewport = camera.viewport().pan_pixels(10.0, 100).unwrap();
    let panned = AlignmentCamera::from_viewport(viewport, 0.0, 12.0, 9.0, clip).unwrap();
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 2,
        camera: panned,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    };
    let (_second, second_stats, second_admission) = renderer
        .prepare_with_admission(&device, &queue, &[request], &ledger, 11)
        .unwrap();
    assert_eq!(second_admission.admitted_pages, 0);
    assert_eq!(
        second_admission.already_resident_pages,
        scene.drawable_page_keys().len()
    );
    assert_eq!(second_stats.geometry_uploaded_bytes, 0);
    assert_eq!(second_stats.page_cache_misses, 0);
}

#[test]
fn prepared_margin_prefetch_makes_the_next_tile_a_camera_only_frame() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene_with_tiled_records(&ledger, NonZeroU64::new(50).unwrap());
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let interaction = AlignmentInteractionState::default();
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 256,
        height: 128,
    };
    let first_camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 0, 50).unwrap(),
        0.0,
        12.0,
        9.0,
        clip,
    )
    .unwrap();
    let first_request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera: first_camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    };
    let (first, _, mandatory) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            std::slice::from_ref(&first_request),
            &ledger,
            10,
        )
        .unwrap();
    assert!(mandatory.admitted_pages > 0);
    let prefetch = renderer
        .prefetch_prepared_scene_pages(
            &device,
            &queue,
            std::slice::from_ref(&first_request),
            &ledger,
            11,
        )
        .unwrap();
    assert!(prefetch.admitted_pages > 0);
    assert_eq!(
        renderer.resident_page_count(),
        scene.drawable_page_keys().len()
    );
    drop(first);

    let second_camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 50, 100).unwrap(),
        0.0,
        12.0,
        9.0,
        clip,
    )
    .unwrap();
    let second_request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 2,
        camera: second_camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    };
    let (_second, second_stats, second_admission) = renderer
        .prepare_with_admission(&device, &queue, &[second_request], &ledger, 12)
        .unwrap();
    assert_eq!(second_admission.admitted_pages, 0);
    assert!(second_admission.already_resident_pages > 0);
    assert_eq!(second_stats.geometry_uploaded_bytes, 0);
}

#[test]
fn prepared_vertical_pages_are_uploaded_before_the_row_camera_reaches_them() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene_with_page_shape(
        &ledger,
        NonZeroU64::new(100).unwrap(),
        4,
        NonZeroU32::new(1).unwrap(),
    );
    assert_eq!(scene.atlas().row_count(), 4);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 5.0,
    };
    let interaction = AlignmentInteractionState::default();
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 256,
        height: 5,
    };
    let first_camera =
        AlignmentCamera::new(scene.prepared_interval(), 0.0, 10.0, 4.0, clip).unwrap();
    assert_eq!(first_camera.visible_rows(), 0..2);
    let first_request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera: first_camera,
        screen_size_points: [256.0, 5.0],
        scissor,
        interaction: &interaction,
    };
    let (first, _, mandatory) = renderer
        .prepare_with_admission(
            &device,
            &queue,
            std::slice::from_ref(&first_request),
            &ledger,
            10,
        )
        .unwrap();
    assert!(mandatory.admitted_pages > 0);
    let prefetch = renderer
        .prefetch_prepared_scene_pages(
            &device,
            &queue,
            std::slice::from_ref(&first_request),
            &ledger,
            11,
        )
        .unwrap();
    assert!(prefetch.admitted_pages > 0);
    assert_eq!(
        renderer.resident_page_count(),
        scene.drawable_page_keys().len()
    );
    drop(first);

    let second_camera =
        AlignmentCamera::new(scene.prepared_interval(), 2.0, 10.0, 4.0, clip).unwrap();
    assert_eq!(second_camera.visible_rows(), 2..4);
    let second_request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 2,
        camera: second_camera,
        screen_size_points: [256.0, 5.0],
        scissor,
        interaction: &interaction,
    };
    let (_second, second_stats, second_admission) = renderer
        .prepare_with_admission(&device, &queue, &[second_request], &ledger, 12)
        .unwrap();
    assert_eq!(second_admission.admitted_pages, 0);
    assert!(second_admission.already_resident_pages > 0);
    assert_eq!(second_stats.geometry_uploaded_bytes, 0);
}

#[test]
fn bounded_prepared_scene_prefetch_advances_deterministically_without_duplicate_uploads() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
    let scene = test_scene_with_tiled_records(&ledger, NonZeroU64::new(20).unwrap());
    assert!(scene.drawable_page_keys().len() > 2);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let camera = AlignmentCamera::new(
        scene.prepared_interval(),
        0.0,
        12.0,
        9.0,
        rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        },
    )
    .unwrap();
    let interaction = AlignmentInteractionState::default();
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let requests = [request];
    let maximum_pages = NonZeroUsize::new(2).unwrap();
    let first = renderer
        .prefetch_prepared_scene_pages_bounded(
            &device,
            &queue,
            &requests,
            &ledger,
            10,
            maximum_pages,
        )
        .unwrap();
    let expected_keys = AlignmentGpuRenderer::prepared_page_keys(&requests);
    assert_eq!(first.prepared_pages, expected_keys.len());
    assert_eq!(first.missing_pages_before, expected_keys.len());
    assert_eq!(first.admission.requested_pages, 2);
    assert_eq!(first.admission.admitted_pages, 2);
    assert_eq!(first.remaining_missing_pages, expected_keys.len() - 2);
    assert!(first.missing_pages_remain);
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>(),
        expected_keys[..2]
    );

    let mut total_admitted = first.admission.admitted_pages;
    let mut total_uploaded = first.admission.uploaded_bytes;
    let mut revision = 11_u64;
    while renderer.resident_page_count() < expected_keys.len() {
        let progress = renderer
            .prefetch_prepared_scene_pages_bounded(
                &device,
                &queue,
                &requests,
                &ledger,
                revision,
                maximum_pages,
            )
            .unwrap();
        assert!(progress.admission.admitted_pages <= maximum_pages.get());
        assert_eq!(progress.admission.already_resident_pages, 0);
        total_admitted = total_admitted.saturating_add(progress.admission.admitted_pages);
        total_uploaded = total_uploaded.saturating_add(progress.admission.uploaded_bytes);
        revision = revision.saturating_add(1);
    }
    assert_eq!(total_admitted, expected_keys.len());
    assert!(total_uploaded > 0);

    let complete = renderer
        .prefetch_prepared_scene_pages_bounded(
            &device,
            &queue,
            &requests,
            &ledger,
            revision,
            maximum_pages,
        )
        .unwrap();
    assert!(complete.is_complete());
    assert_eq!(complete.missing_pages_before, 0);
    assert_eq!(complete.admission.requested_pages, 0);
    assert_eq!(complete.admission.admitted_pages, 0);
    assert_eq!(complete.admission.uploaded_bytes, 0);
    assert_eq!(renderer.resident_page_count(), expected_keys.len());
}

#[test]
fn prepared_scene_prefetch_cursor_resolves_once_then_performs_only_bounded_work() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
    let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(20).unwrap());
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let interaction = AlignmentInteractionState::default();
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera: AlignmentCamera::new(
            scene.prepared_interval(),
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 256.0,
                height: 128.0,
            },
        )
        .unwrap(),
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let requests = [request];
    let keys: Arc<[AlignmentPageKey]> = AlignmentGpuRenderer::prepared_page_keys(&requests).into();
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

    let mut revision = 20_u64;
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
        Err(AlignmentGpuPrefetchCursorError::ResidencyBudgetLimited {
            maximum_total_pinned_bytes: 0,
            ..
        })
    ));
}

#[test]
fn completed_scene_residency_lease_requires_every_page_and_pins_until_drop() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
    let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(50).unwrap());
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let camera = AlignmentCamera::new(
        scene.prepared_interval(),
        0.0,
        12.0,
        9.0,
        rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        },
    )
    .unwrap();
    let interaction = AlignmentInteractionState::default();
    let requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    }];

    assert!(matches!(
        renderer.pin_prepared_scene_pages(&requests),
        Err(AlignmentGpuResidencyError::MissingPreparedPages {
            missing,
            ..
        }) if missing == scene.drawable_page_keys().len()
    ));
    renderer
        .prefetch_prepared_scene_pages(&device, &queue, &requests, &ledger, 10)
        .unwrap();
    let lease = renderer.pin_prepared_scene_pages(&requests).unwrap();
    assert_eq!(lease.device_generation(), renderer.device_generation);
    assert_eq!(lease.page_count(), scene.drawable_page_keys().len());
    assert!(!lease.is_empty());
    assert!(lease.page_keys().windows(2).all(|pair| pair[0] < pair[1]));

    let resident = renderer.resident_page_count();
    let snapshot = ledger.snapshot().unwrap();
    let retained = renderer.resident_retained_bytes().unwrap();
    let pressure = GpuBudgetPressure::from_budget_failure(
        retained,
        snapshot.charged,
        snapshot.reserved,
        snapshot.charged.saturating_add(snapshot.reserved),
    );
    let pinned = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();
    assert_eq!(pinned.pressure, pressure);
    assert_eq!(pinned.reclaimed_pages(), 0);
    assert!(!pinned.headroom_satisfied);
    assert_eq!(renderer.resident_page_count(), resident);
    drop(lease);
    let released = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();
    assert_eq!(released.reclaimed_pages(), resident);
    assert_eq!(renderer.resident_page_count(), 0);
}

#[test]
fn preparing_a_replacement_does_not_overwrite_an_in_flight_frame() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene(&ledger);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    renderer
        .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
        .unwrap();
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 256,
        height: 128,
    };
    let first_request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    };
    let (first, _) = renderer
        .prepare(&device, &queue, &[first_request], &ledger)
        .unwrap();
    let panned_viewport = camera.viewport().pan_pixels(10.0, 100).unwrap();
    let panned = AlignmentCamera::from_viewport(panned_viewport, 0.0, 12.0, 9.0, clip).unwrap();
    let second_request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 2,
        camera: panned,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    };
    let (second, stats) = renderer
        .prepare(&device, &queue, &[second_request], &ledger)
        .unwrap();
    assert_eq!(stats.geometry_uploaded_bytes, 0);
    let cpu =
        CpuAlignmentProjector::project_with_interaction(&scene, 2, panned, &interaction).unwrap();
    let cpu_labels = cpu
        .primitives
        .iter()
        .filter(|primitive| primitive.shape == rgv_alignment_scene::CpuPrimitiveShape::TextLabel)
        .cloned()
        .collect::<Vec<_>>();
    assert!(!cpu_labels.is_empty());
    assert_eq!(second.label_frames().len(), 1);
    assert_eq!(
        second.label_frames()[0].labels.as_ref(),
        cpu_labels.as_slice()
    );
    assert_eq!(stats.text_labels, cpu_labels.len());
    assert_eq!(
        stats.label_anchors_examined,
        second.label_frames()[0].anchors_examined
    );
    assert!(stats.label_anchors_examined >= stats.text_labels);
    assert_ne!(first.revision(), second.revision());
    render_test_frame(&device, &queue, &renderer, &first).unwrap();
    render_test_frame(&device, &queue, &renderer, &second).unwrap();

    let snapshot = ledger.snapshot().unwrap();
    assert!(
        snapshot
            .records
            .iter()
            .any(|record| { record.class == AllocationClass::TransitionFrame && record.pins > 0 })
    );
    assert!(renderer.frame_buffers.len() >= 2);
}

#[test]
fn camera_scroll_and_hover_captures_preserve_read_content_authority_without_a_bundle() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_scene_with_page_shape(
        &ledger,
        NonZeroU64::new(20).unwrap(),
        8,
        NonZeroU32::new(2).unwrap(),
    );
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 24.0,
    };
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 256,
        height: 24,
    };
    let first_camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 0, 40).unwrap(),
        0.0,
        12.0,
        9.0,
        clip,
    )
    .unwrap();
    let neutral = AlignmentInteractionState::default();
    let first_requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera: first_camera,
        screen_size_points: [256.0, 24.0],
        scissor,
        interaction: &neutral,
    }];
    let (first, _, _) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        None,
        700,
        800,
    );
    renderer
        .prefetch_prepared_scene_pages(&device, &queue, &first_requests, &ledger, 801)
        .unwrap();
    let authority = first.content_authority().clone();
    assert_eq!(
        authority.page_manifest().len(),
        scene.drawable_page_keys().len()
    );
    drop(first);

    let page_allocations = renderer
        .cache
        .pages
        .iter()
        .map(|page| (page.key, page.allocation.id()))
        .collect::<Vec<_>>();
    let page_table_allocation = renderer
        .cache
        .page_table_allocation
        .as_ref()
        .map(AllocationLease::id);
    let hovered = scene
        .pages()
        .iter()
        .flat_map(|page| page.primitives())
        .next()
        .expect("the overlapping read fixture produces semantic primitives")
        .handle;
    let interactions = [
        AlignmentInteractionState::default(),
        AlignmentInteractionState::default(),
        AlignmentInteractionState::new(Some(hovered), None, []),
    ];
    let cameras = [
        AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 5, 45).unwrap(),
            0.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap(),
        AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 10, 50).unwrap(),
            2.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap(),
        AlignmentCamera::new(
            GenomicInterval::new(ContigId::new(0), 15, 55).unwrap(),
            1.0,
            12.0,
            9.0,
            clip,
        )
        .unwrap(),
    ];
    let operations_before = ledger.operation_counters().unwrap();
    let renderer_operations_before = renderer.operation_counters();
    let mut presented = None;
    let mut page_pin_acquisitions = Vec::new();
    for (index, (camera, interaction)) in cameras.into_iter().zip(&interactions).enumerate() {
        assert_eq!(ledger.progress().unwrap().active_replacement_bundle, None);
        let requests = [AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 2 + index as u64,
            camera,
            screen_size_points: [256.0, 24.0],
            scissor,
            interaction,
        }];
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
                .map(|page| (page.key, page.allocation.id()))
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
    let repeated_requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 99,
        camera: cameras[2],
        screen_size_points: [256.0, 24.0],
        scissor,
        interaction: &interactions[2],
    }];
    let (repeated, repeated_stats) = renderer
        .capture_camera_frame(&device, &queue, &authority, &repeated_requests, &ledger)
        .unwrap();
    assert_eq!(
        repeated_stats.page_pin_acquisitions, 0,
        "a contained frame must reuse the prior presentation's page leases"
    );
    let previous = presented.replace(repeated);
    drop(previous);
    let operations_after = ledger.operation_counters().unwrap();
    let renderer_operations_after = renderer.operation_counters();
    assert_eq!(
        operations_after.full_inventory_snapshots, operations_before.full_inventory_snapshots,
        "camera, vertical-scroll, and hover capture must not inventory retained allocations"
    );
    assert_eq!(
        operations_after.allocation_records_enumerated,
        operations_before.allocation_records_enumerated
    );
    assert_eq!(
        renderer_operations_after, renderer_operations_before,
        "camera, vertical-scroll, and hover capture must not rebuild the full prepared-page manifest"
    );
    assert!(renderer.frame_buffers.len() <= 2);

    let changed_scene_set = [
        AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 7,
            camera: first_camera,
            screen_size_points: [256.0, 24.0],
            scissor,
            interaction: &neutral,
        },
        AlignmentGpuSceneRequest {
            scene: &scene,
            camera_sequence: 8,
            camera: first_camera,
            screen_size_points: [256.0, 24.0],
            scissor,
            interaction: &neutral,
        },
    ];
    assert!(matches!(
        renderer.capture_camera_frame(&device, &queue, &authority, &changed_scene_set, &ledger,),
        Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
            reason: AlignmentGpuContentReplacementReason::SceneContent { .. }
        })
    ));

    let changed_manifest_scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(25).unwrap());
    assert_eq!(changed_manifest_scene.revision(), scene.revision());
    let changed_requests = [AlignmentGpuSceneRequest {
        scene: &changed_manifest_scene,
        camera_sequence: 9,
        camera: first_camera,
        screen_size_points: [256.0, 24.0],
        scissor,
        interaction: &neutral,
    }];
    assert!(matches!(
        renderer.capture_camera_frame(&device, &queue, &authority, &changed_requests, &ledger,),
        Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
            reason: AlignmentGpuContentReplacementReason::SceneContent { .. }
        })
    ));
    let (replacement, replacement_stats, _) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &changed_requests,
        &ledger,
        None,
        740,
        840,
    );
    assert!(replacement_stats.geometry_uploaded_bytes > 0);

    let replacement_authority = replacement.content_authority().clone();
    drop(replacement);
    renderer.discard_device_resources();
    assert!(matches!(
        renderer.capture_camera_frame(
            &device,
            &queue,
            &replacement_authority,
            &changed_requests,
            &ledger,
        ),
        Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired {
            reason: AlignmentGpuContentReplacementReason::DeviceGeneration { .. }
        })
    ));
}

#[test]
fn predicate_highlight_uses_page_local_interaction_buffers() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 8 << 20, 16 << 20);
    let scene = test_scene_with_page_shape(
        &ledger,
        NonZeroU64::new(20).unwrap(),
        8,
        NonZeroU32::new(2).unwrap(),
    );
    let highlighted_handles = scene
        .pages()
        .iter()
        .flat_map(|page| page.primitives())
        .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
        .map(|primitive| primitive.handle)
        .collect::<BTreeSet<_>>();
    assert!(highlighted_handles.len() > 3);
    let interaction =
        AlignmentInteractionState::default().with_highlighted(highlighted_handles.iter().copied());
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let (frame, stats, _) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &[request],
        &ledger,
        None,
        760,
        860,
    );

    assert!(stats.overlay_instances > 0);
    assert!(
        frame
            .draws
            .iter()
            .any(|draw| draw.interaction_buffer_index.is_some())
    );
    let visible_rows = camera.visible_rows();
    let camera_page_envelope = scene.camera_page_envelope(
        camera.interval().len(),
        visible_rows.end.saturating_sub(visible_rows.start),
    );
    assert!(renderer.frame_buffers.iter().all(|slot| {
        slot.interaction_buffers.len() >= camera_page_envelope
            && slot.interaction_capacity >= scene.maximum_non_label_primitives_per_page()
    }));
    assert!(renderer.frame_buffers.iter().all(|slot| {
        slot.interaction_capacity
            .saturating_mul(size_of::<AlignmentGpuInteractionInstance>())
            <= device.limits().max_buffer_size as usize
    }));
    render_test_frame(&device, &queue, &renderer, &frame).unwrap();
}

#[test]
fn compatible_read_capture_uses_the_precharged_ring_while_an_unrelated_bundle_is_active() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 12 << 20, 16 << 20);
    let scene = test_scene_with_tiled_records(&ledger, NonZeroU64::new(20).unwrap());
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 512.0,
        height: 256.0,
    };
    let first_camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 0, 20).unwrap(),
        0.0,
        12.0,
        9.0,
        clip,
    )
    .unwrap();
    let neutral = AlignmentInteractionState::default();
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 512,
        height: 256,
    };
    let first_requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera: first_camera,
        screen_size_points: [512.0, 256.0],
        scissor,
        interaction: &neutral,
    }];
    let (first, _, _) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        None,
        900,
        901,
    );
    renderer
        .prefetch_prepared_scene_pages(&device, &queue, &first_requests, &ledger, 902)
        .unwrap();
    let authority = first.content_authority().clone();
    assert_eq!(
        renderer.frame_buffers.len(),
        ALIGNMENT_CAMERA_FRAME_SLOT_COUNT
    );
    let slot_ids = renderer
        .frame_buffers
        .iter()
        .map(|slot| {
            (
                slot.camera_allocation.as_ref().map(AllocationLease::id),
                slot.interaction_allocation
                    .as_ref()
                    .map(AllocationLease::id),
                slot.frame_allocation.id(),
            )
        })
        .collect::<Vec<_>>();

    let handles = scene
        .pages()
        .iter()
        .flat_map(|page| page.primitives())
        .map(|primitive| primitive.handle)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    assert!(handles.len() >= 3);
    let interaction = AlignmentInteractionState::new(
        handles.first().copied(),
        handles.get(1).copied(),
        handles.iter().copied(),
    );
    let wide_camera =
        AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 2,
        camera: wide_camera,
        screen_size_points: [512.0, 256.0],
        scissor,
        interaction: &interaction,
    }];

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
    assert!(stats.overlay_instances > 0);
    assert_eq!(stats.geometry_uploaded_bytes, 0);
    assert_ne!(
        first.buffers.frame_allocation.id(),
        second.buffers.frame_allocation.id()
    );
    assert!(matches!(
        renderer.capture_camera_frame(&device, &queue, &authority, &requests, &ledger),
        Err(AlignmentGpuCameraCaptureError::FrameSlotBusy { slots: 2 })
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
                slot.camera_allocation.as_ref().map(AllocationLease::id),
                slot.interaction_allocation
                    .as_ref()
                    .map(AllocationLease::id),
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
fn large_read_horizon_interaction_slots_scale_with_the_viewport_not_the_horizon() {
    let Some((device, _queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(128 << 20, 16 << 20, 32 << 20);
    let scene = test_scene_with_page_shape(
        &ledger,
        NonZeroU64::new(20).unwrap(),
        2_048,
        NonZeroU32::new(32).unwrap(),
    );
    let renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 0, 20).unwrap(),
        0.0,
        12.0,
        9.0,
        rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 512.0,
            height: 256.0,
        },
    )
    .unwrap();
    let highlighted = scene
        .pages()
        .iter()
        .flat_map(|page| page.primitives())
        .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
        .map(|primitive| primitive.handle)
        .collect::<BTreeSet<_>>();
    let interaction =
        AlignmentInteractionState::default().with_highlighted(highlighted.iter().copied());
    let requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [512.0, 256.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 512,
            height: 256,
        },
        interaction: &interaction,
    }];
    let plan = renderer
        .plan_replacement(
            &requests,
            &ledger.snapshot().unwrap(),
            AllocationBundleSlot::new(1_000),
        )
        .unwrap();
    let full_non_label_geometry = scene
        .pages()
        .iter()
        .flat_map(|page| page.primitives())
        .filter(|primitive| !matches!(primitive.component, AlignmentComponent::Label { .. }))
        .count();
    assert!(full_non_label_geometry > 2_000);
    let visible_rows = camera.visible_rows();
    let camera_envelope = scene.camera_page_envelope(
        camera.interval().len(),
        visible_rows.end.saturating_sub(visible_rows.start),
    );
    assert_eq!(plan.camera_capacity, camera_envelope.next_power_of_two());
    assert!(plan.camera_capacity < scene.drawable_page_keys().len());
    assert!(plan.interaction_capacity > 0);
    assert_eq!(plan.interaction_buffer_count, camera_envelope);
    assert_eq!(
        plan.interaction_capacity,
        scene
            .maximum_non_label_primitives_per_page()
            .next_power_of_two()
    );
    assert!(plan.interaction_capacity < full_non_label_geometry);
    assert_eq!(
        plan.interaction_buffer_bytes,
        plan.interaction_capacity
            .saturating_mul(size_of::<AlignmentGpuInteractionInstance>())
    );
    assert!(plan.interaction_buffer_bytes <= device.limits().max_buffer_size as usize);
    assert!(plan.maximum_interactions > 0);
    assert!(
        plan.interaction_total_bytes
            < full_non_label_geometry.saturating_mul(size_of::<AlignmentGpuInstance>()),
        "interaction capacity must remain smaller than the former full-horizon geometry copy"
    );
    assert_eq!(plan.frame_slots.len(), ALIGNMENT_CAMERA_FRAME_SLOT_COUNT);
}

#[test]
fn active_frame_pins_prevent_gpu_page_eviction_until_submission_releases() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene(&ledger);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    renderer
        .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
        .unwrap();
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let (frame, _) = renderer
        .prepare(&device, &queue, &[request], &ledger)
        .unwrap();
    let resident = renderer.resident_page_count();
    let snapshot = ledger.snapshot().unwrap();
    let retained = renderer.resident_retained_bytes().unwrap();
    let pressure = GpuBudgetPressure::from_budget_failure(
        retained,
        snapshot.charged,
        snapshot.reserved,
        snapshot.charged.saturating_add(snapshot.reserved),
    );
    let pinned = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();
    assert_eq!(pinned.reclaimed_pages(), 0);
    assert_eq!(renderer.resident_page_count(), resident);
    assert_eq!(renderer.frame_buffers.len(), 1);
    render_test_frame(&device, &queue, &renderer, &frame).unwrap();
    drop(frame);
    let released = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();
    assert_eq!(released.reclaimed_pages(), resident);
    assert_eq!(renderer.resident_page_count(), 0);
    assert!(renderer.frame_buffers.is_empty());
}

#[test]
fn cooperative_reclaim_drops_idle_resources_and_reports_renderer_local_bytes() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
    let scene = test_scene(&ledger);
    let unrelated_bytes = 4_096;
    let _unrelated = ledger
        .reserve(
            AllocationClass::GpuPage,
            unrelated_bytes,
            ReservationKind::Normal,
        )
        .unwrap()
        .register(unrelated_bytes, 1)
        .unwrap();
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    renderer
        .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
        .unwrap();
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let request = |camera_sequence| AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let (first, _) = renderer
        .prepare(&device, &queue, &[request(1)], &ledger)
        .unwrap();
    let (second, _) = renderer
        .prepare(&device, &queue, &[request(2)], &ledger)
        .unwrap();
    assert_eq!(renderer.frame_buffers.len(), 2);
    drop(first);
    drop(second);

    let before = ledger.snapshot().unwrap();
    let retained_before = renderer.resident_retained_bytes().unwrap();
    assert!(retained_before > 0);
    let available = before
        .budget
        .saturating_sub(before.charged.saturating_add(before.reserved));
    let pressure = GpuBudgetPressure::from_budget_failure(
        available.saturating_add(retained_before),
        before.charged,
        before.reserved,
        before.budget,
    );
    let report = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();

    assert_eq!(report.pressure, pressure);
    assert_eq!(report.pages_before, scene.drawable_page_keys().len());
    assert_eq!(report.pages_after, 0);
    assert_eq!(report.retained_bytes_before, retained_before);
    assert_eq!(report.retained_bytes_after, 0);
    assert_eq!(report.ledger_charged_before, before.charged);
    assert_eq!(report.released_retained_bytes(), retained_before);
    assert_eq!(report.released_ledger_bytes(), retained_before);
    assert!(report.headroom_satisfied);
    assert!(renderer.frame_buffers.is_empty());
    assert_eq!(renderer.cache.page_table_capacity, 0);
    assert!(renderer.cache.page_table_allocation.is_none());

    let global_gpu_bytes = ledger
        .snapshot()
        .unwrap()
        .records
        .iter()
        .filter(|record| record.class == AllocationClass::GpuPage)
        .map(|record| record.charged_capacity)
        .sum::<usize>();
    assert_eq!(global_gpu_bytes, unrelated_bytes);
}

#[test]
fn cooperative_reclaim_skips_protected_and_pinned_pages_in_deterministic_lru_order() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
    let scene = test_scene_with_tiled_records(&ledger, NonZeroU64::new(20).unwrap());
    assert!(scene.drawable_page_keys().len() > 2);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    renderer
        .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
        .unwrap();
    for page in &mut renderer.cache.pages {
        page.last_used_revision = 7;
    }
    renderer.rebuild_reclaim_index();
    let protected = renderer.cache.pages[0].key;
    let pinned_key = renderer.cache.pages[1].key;
    let pin = renderer.page(pinned_key).unwrap().allocation.pin().unwrap();
    let eligible_order = renderer
        .cache
        .pages
        .iter()
        .filter(|page| page.key != protected && page.key != pinned_key)
        .map(|page| page.key)
        .collect::<Vec<_>>();
    assert!(eligible_order.len() > 1);
    let eligible_bytes = renderer
        .cache
        .pages
        .iter()
        .filter(|page| page.key != protected && page.key != pinned_key)
        .map(|page| page.allocation.snapshot().unwrap().charged_capacity)
        .sum::<usize>();
    let before = ledger.snapshot().unwrap();
    let pressure = GpuBudgetPressure::from_budget_failure(
        eligible_bytes,
        before.charged,
        before.reserved,
        before.charged.saturating_add(before.reserved),
    );
    let report = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[protected])
        .unwrap();
    let remaining = renderer
        .cache
        .pages
        .iter()
        .map(|page| page.key)
        .collect::<Vec<_>>();
    let removed = eligible_order
        .iter()
        .copied()
        .filter(|key| !remaining.contains(key))
        .collect::<Vec<_>>();

    assert!(removed.len() > 1);
    assert_eq!(removed.as_slice(), &eligible_order[..removed.len()]);
    assert_eq!(report.reclaimed_pages(), removed.len());
    assert!(remaining.contains(&protected));
    assert!(remaining.contains(&pinned_key));
    assert!(
        renderer
            .page(pinned_key)
            .unwrap()
            .allocation
            .snapshot()
            .unwrap()
            .pins
            > 0
    );
    drop(pin);
}

#[test]
fn infeasible_reclaim_pressure_leaves_the_scene_cache_untouched() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
    let scene = test_scene_with_tile_bases(&ledger, NonZeroU64::new(25).unwrap());
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    renderer
        .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
        .unwrap();
    let page_keys_before = renderer
        .cache
        .pages
        .iter()
        .map(|page| page.key)
        .collect::<Vec<_>>();
    let table_before = renderer
        .cache
        .page_table_allocation
        .as_ref()
        .map(AllocationLease::id);
    let table_capacity_before = renderer.cache.page_table_capacity;
    let retained_before = renderer.resident_retained_bytes().unwrap();
    let ledger_before = ledger.snapshot().unwrap();
    let pressure = GpuBudgetPressure::from_budget_failure(
        ledger_before.budget.saturating_add(1),
        ledger_before.charged,
        ledger_before.reserved,
        ledger_before.budget,
    );

    let report = renderer
        .reclaim_for_budget_pressure(&ledger, pressure, &[])
        .unwrap();

    assert!(
        !GpuReclaimGoal::new(
            pressure,
            ledger_before.charged.saturating_add(ledger_before.reserved)
        )
        .is_feasible()
    );
    assert_eq!(report.reclaimed_pages(), 0);
    assert!(!report.headroom_satisfied);
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| page.key)
            .collect::<Vec<_>>(),
        page_keys_before
    );
    assert_eq!(
        renderer
            .cache
            .page_table_allocation
            .as_ref()
            .map(AllocationLease::id),
        table_before
    );
    assert_eq!(renderer.cache.page_table_capacity, table_capacity_before);
    assert_eq!(renderer.resident_retained_bytes().unwrap(), retained_before);
    assert_eq!(ledger.snapshot().unwrap().charged, ledger_before.charged);
}

#[test]
fn exact_transition_pressure_does_not_reclaim_when_the_original_limit_already_fits() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let transition_reserve = 20;
    let ledger =
        AllocationLedger::new_with_transient_budget(32 << 20, transition_reserve, 16 << 20);
    let scene = test_scene(&ledger);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    renderer
        .admit_pages(&device, &queue, scene.pages(), &ledger, 10)
        .unwrap();
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
    assert_eq!(report.reclaimed_pages(), 0);
    assert_eq!(report.released_retained_bytes(), 0);
    assert_eq!(renderer.resident_page_count(), pages_before);
    assert_eq!(renderer.resident_retained_bytes().unwrap(), retained_before);
    assert!(report.ledger_charged_before > base_budget);
    assert_eq!(report.ledger_charged_after, report.ledger_charged_before);
}

#[test]
fn read_gpu_stats_exclude_unrelated_shared_ledger_gpu_allocations() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(32 << 20, 4 << 20, 16 << 20);
    let scene = test_scene(&ledger);
    let unrelated_bytes = 4_096;
    let _unrelated = ledger
        .reserve(
            AllocationClass::GpuPage,
            unrelated_bytes,
            ReservationKind::Normal,
        )
        .unwrap()
        .register(unrelated_bytes, 1)
        .unwrap();
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let camera = AlignmentCamera::new(
        scene.prepared_interval(),
        0.0,
        12.0,
        9.0,
        rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 256.0,
            height: 128.0,
        },
    )
    .unwrap();
    let interaction = AlignmentInteractionState::default();
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let (_frame, stats, _) = renderer
        .prepare_with_admission(&device, &queue, &[request], &ledger, 10)
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
fn first_bundled_frame_cannot_spend_transition_reserve_above_the_base_budget() {
    let Some((device, _queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let scene = test_scene(&ledger);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    }];
    let initial = ledger.snapshot().unwrap();
    let preliminary = renderer
        .plan_replacement(&requests, &initial, AllocationBundleSlot::new(7))
        .unwrap();
    let candidate_bytes = preliminary
        .demands()
        .iter()
        .try_fold(0_usize, |total, demand| {
            total.checked_add(demand.maximum_bytes)
        })
        .unwrap();
    assert!(candidate_bytes > 0);
    let occupied = initial.charged.saturating_add(initial.reserved);
    ledger
        .set_limits(
            occupied.saturating_add(candidate_bytes).saturating_sub(1),
            candidate_bytes,
            initial.transient_budget,
        )
        .unwrap();

    let snapshot = ledger.snapshot().unwrap();
    let plan = renderer
        .plan_replacement(&requests, &snapshot, AllocationBundleSlot::new(7))
        .unwrap();
    let mut prior = None;
    let retirement = renderer
        .detach_replacement_retirement(&ledger, &plan, &mut prior)
        .unwrap();
    assert!(retirement.retirement_claim_set().is_empty());
    let spec = AllocationBundleSpec::new(
        plan.policy_revision(),
        plan.demands().to_vec(),
        retirement.retirement_claim_set().clone(),
    );
    assert!(matches!(
        ledger.reserve_replacement_bundle(spec),
        Err(AllocationBundleError::SteadyBudgetExceeded { .. })
    ));
    assert!(
        renderer
            .restore_replacement_retirement(retirement)
            .unwrap()
            .is_none()
    );
    assert_eq!(renderer.resident_page_count(), 0);
    assert!(renderer.frame_buffers.is_empty());
}

#[test]
fn retirement_preflight_error_preserves_the_caller_frame_and_renderer_cache() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let foreign_ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let scene = test_scene(&ledger);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    }];
    let (frame, _, _) = renderer
        .prepare_with_admission(&device, &queue, &requests, &ledger, 72)
        .unwrap();
    let snapshot = ledger.snapshot().unwrap();
    let plan = renderer
        .plan_replacement(&requests, &snapshot, AllocationBundleSlot::new(13))
        .unwrap();
    let renderer_revision = renderer.revision;
    let page_ids = renderer
        .cache
        .pages
        .iter()
        .map(|page| page.allocation.id())
        .collect::<Vec<_>>();
    let table = (
        renderer
            .cache
            .page_table_allocation
            .as_ref()
            .map(AllocationLease::id),
        renderer.cache.page_table_capacity,
    );
    let buffer_identity = renderer
        .frame_buffers
        .iter()
        .map(Arc::as_ptr)
        .collect::<Vec<_>>();
    let frame_id = frame.buffers.frame_allocation.id();
    let mut prior = Some(frame);

    assert!(matches!(
        renderer.detach_replacement_retirement(&foreign_ledger, &plan, &mut prior),
        Err(AlignmentGpuReplacementError::Bundle(
            AllocationBundleError::ForeignRetirementReference { .. }
        ))
    ));
    assert_eq!(renderer.revision, renderer_revision);
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| page.allocation.id())
            .collect::<Vec<_>>(),
        page_ids
    );
    assert_eq!(
        (
            renderer
                .cache
                .page_table_allocation
                .as_ref()
                .map(AllocationLease::id),
            renderer.cache.page_table_capacity,
        ),
        table
    );
    assert_eq!(
        renderer
            .frame_buffers
            .iter()
            .map(Arc::as_ptr)
            .collect::<Vec<_>>(),
        buffer_identity
    );
    let prior = prior
        .as_ref()
        .expect("preflight refusal must not consume it");
    assert_eq!(prior.buffers.frame_allocation.id(), frame_id);
    render_test_frame(&device, &queue, &renderer, prior).unwrap();
}

#[test]
fn over_base_bundled_replacement_refusal_restores_the_exact_prior_frame_and_cache() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let old_scene = test_scene(&ledger);
    let candidate_scene = test_scene_with_tiled_records(&ledger, NonZeroU64::new(25).unwrap());
    assert!(candidate_scene.drawable_page_keys().len() >= 4);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let interaction = AlignmentInteractionState::default();
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 256,
        height: 128,
    };
    let old_camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 0, 25).unwrap(),
        0.0,
        12.0,
        9.0,
        clip,
    )
    .unwrap();
    let old_requests = [AlignmentGpuSceneRequest {
        scene: &old_scene,
        camera_sequence: 1,
        camera: old_camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    }];
    let (prior, _, _) = renderer
        .prepare_with_admission(&device, &queue, &old_requests, &ledger, 70)
        .unwrap();
    let renderer_revision = renderer.revision;
    let pages_before = renderer
        .cache
        .pages
        .iter()
        .map(|page| {
            (
                page.key,
                page.allocation.id(),
                page.instance_count,
                page.last_used_revision,
            )
        })
        .collect::<Vec<_>>();
    let table_before = (
        renderer
            .cache
            .page_table_allocation
            .as_ref()
            .map(AllocationLease::id),
        renderer.cache.page_table_capacity,
    );
    let frame_buffers_before = renderer
        .frame_buffers
        .iter()
        .map(|buffers| {
            (
                Arc::as_ptr(buffers),
                buffers.camera_allocation.as_ref().map(AllocationLease::id),
                buffers
                    .interaction_allocation
                    .as_ref()
                    .map(AllocationLease::id),
                buffers.camera_capacity,
                buffers.interaction_buffers.len(),
                buffers.interaction_capacity,
            )
        })
        .collect::<Vec<_>>();
    let prior_frame_allocation = prior.buffers.frame_allocation.id();
    let prior_buffer = Arc::as_ptr(&prior.buffers);

    let before = ledger.snapshot().unwrap();
    ledger.set_budget(before.charged).unwrap();
    let new_camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 25, 100).unwrap(),
        0.0,
        12.0,
        9.0,
        clip,
    )
    .unwrap();
    let new_requests = [AlignmentGpuSceneRequest {
        scene: &candidate_scene,
        camera_sequence: 2,
        camera: new_camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    }];
    let snapshot = ledger.snapshot().unwrap();
    let plan = renderer
        .plan_replacement(&new_requests, &snapshot, AllocationBundleSlot::new(31))
        .unwrap();
    assert!(plan.missing_pages.len() > pages_before.len());
    assert!(plan.page_table_slot.is_some());
    let mut prior = Some(prior);
    let retirement = renderer
        .detach_replacement_retirement(&ledger, &plan, &mut prior)
        .unwrap();
    assert!(prior.is_none());
    assert_eq!(retirement.retired_pages.len(), pages_before.len());
    assert!(retirement.retired_page_table.is_some());
    assert_eq!(retirement.retired_frame_buffers.len(), 1);
    assert!(renderer.cache.pages.is_empty());
    assert!(renderer.cache.page_table_allocation.is_none());
    assert!(renderer.frame_buffers.is_empty());
    let spec = AllocationBundleSpec::new(
        plan.policy_revision(),
        plan.demands().to_vec(),
        retirement.retirement_claim_set().clone(),
    );
    assert!(matches!(
        ledger.reserve_replacement_bundle(spec),
        Err(AllocationBundleError::SteadyBudgetExceeded { .. })
    ));

    let restored = renderer
        .restore_replacement_retirement(retirement)
        .unwrap()
        .unwrap();
    assert_eq!(renderer.revision, renderer_revision);
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| {
                (
                    page.key,
                    page.allocation.id(),
                    page.instance_count,
                    page.last_used_revision,
                )
            })
            .collect::<Vec<_>>(),
        pages_before
    );
    assert_eq!(
        (
            renderer
                .cache
                .page_table_allocation
                .as_ref()
                .map(AllocationLease::id),
            renderer.cache.page_table_capacity,
        ),
        table_before
    );
    assert_eq!(
        renderer
            .frame_buffers
            .iter()
            .map(|buffers| {
                (
                    Arc::as_ptr(buffers),
                    buffers.camera_allocation.as_ref().map(AllocationLease::id),
                    buffers
                        .interaction_allocation
                        .as_ref()
                        .map(AllocationLease::id),
                    buffers.camera_capacity,
                    buffers.interaction_buffers.len(),
                    buffers.interaction_capacity,
                )
            })
            .collect::<Vec<_>>(),
        frame_buffers_before
    );
    assert_eq!(
        restored.buffers.frame_allocation.id(),
        prior_frame_allocation
    );
    assert_eq!(Arc::as_ptr(&restored.buffers), prior_buffer);
    render_test_frame(&device, &queue, &renderer, &restored).unwrap();
}

#[test]
fn bundled_constant_camera_reuses_prepared_geometry_without_uploading_it_again() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let scene = test_scene(&ledger);
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let request = |camera_sequence| AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let first_requests = [request(1)];
    let (first, first_stats, first_admission) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        None,
        80,
        90,
    );
    assert!(first_admission.admitted_pages > 0);
    assert!(first_stats.geometry_uploaded_bytes > 0);

    let second_requests = [request(1)];
    let (second, second_stats, second_admission) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &second_requests,
        &ledger,
        Some(first),
        100,
        91,
    );
    assert_eq!(second_admission.admitted_pages, 0);
    assert_eq!(
        second_admission.already_resident_pages,
        scene.drawable_page_keys().len()
    );
    assert_eq!(second_stats.geometry_uploaded_bytes, 0);
    assert_eq!(second_stats.page_cache_misses, 0);
    render_test_frame(&device, &queue, &renderer, &second).unwrap();
}

#[test]
fn bundled_replacements_preserve_multi_quantum_prepared_prefetch_without_repeat_uploads() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(64 << 20, 16 << 20, 16 << 20);
    let scene = test_scene_with_tiled_records(&ledger, NonZeroU64::new(20).unwrap());
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let interaction = AlignmentInteractionState::default();
    let scissor = AlignmentGpuScissor {
        x: 0,
        y: 0,
        width: 256,
        height: 128,
    };
    let first_camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 0, 20).unwrap(),
        0.0,
        12.0,
        9.0,
        clip,
    )
    .unwrap();
    let first_requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera: first_camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    }];
    let prepared_keys = AlignmentGpuRenderer::prepared_page_keys(&first_requests);
    assert!(prepared_keys.len() >= 5);
    let (first, _, first_admission) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &first_requests,
        &ledger,
        None,
        180,
        300,
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
                301 + admitted_quanta as u64,
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
        .map(|page| (page.key, page.allocation.id()))
        .collect::<Vec<_>>();

    let repeated_requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 2,
        camera: first_camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    }];
    let (second, second_stats, second_admission) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &repeated_requests,
        &ledger,
        Some(first),
        220,
        320,
    );
    assert_eq!(second_admission.admitted_pages, 0);
    assert_eq!(second_stats.geometry_uploaded_bytes, 0);
    assert_eq!(renderer.resident_page_count(), prepared_keys.len());
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| (page.key, page.allocation.id()))
            .collect::<Vec<_>>(),
        converged_page_ids
    );
    let no_repeat = renderer
        .prefetch_prepared_scene_pages_bounded(
            &device,
            &queue,
            &repeated_requests,
            &ledger,
            321,
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
    assert!(no_repeat.is_complete());
    assert_eq!(no_repeat.admission.admitted_pages, 0);
    assert_eq!(no_repeat.admission.uploaded_bytes, 0);

    let compatible_camera = AlignmentCamera::new(
        GenomicInterval::new(ContigId::new(0), 20, 40).unwrap(),
        0.0,
        12.0,
        9.0,
        clip,
    )
    .unwrap();
    let compatible_requests = [AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 3,
        camera: compatible_camera,
        screen_size_points: [256.0, 128.0],
        scissor,
        interaction: &interaction,
    }];
    let (third, third_stats, third_admission) = publish_bundled_frame(
        &device,
        &queue,
        &mut renderer,
        &compatible_requests,
        &ledger,
        Some(second),
        260,
        322,
    );
    assert_eq!(third_admission.admitted_pages, 0);
    assert_eq!(third_stats.geometry_uploaded_bytes, 0);
    assert_eq!(renderer.resident_page_count(), prepared_keys.len());
    assert_eq!(
        renderer
            .cache
            .pages
            .iter()
            .map(|page| (page.key, page.allocation.id()))
            .collect::<Vec<_>>(),
        converged_page_ids
    );
    render_test_frame(&device, &queue, &renderer, &third).unwrap();
}

#[test]
fn only_persistent_budget_failures_report_retryable_gpu_pressure() {
    let budget = LedgerError::BudgetExceeded {
        requested: 4_096,
        charged: 8_192,
        reserved: 0,
        limit: 12_048,
    };
    let expected = GpuBudgetPressure::from_budget_failure(4_096, 8_192, 0, 12_048);
    let admission =
        AlignmentGpuFrameBuildError::Admission(AlignmentGpuPageAdmissionError::Ledger(budget));
    assert_eq!(admission.persistent_budget_pressure(), Some(expected));
    assert_eq!(expected.required_reclaim_bytes(), 240);

    let nested = AlignmentGpuFrameBuildError::Prepare(AlignmentGpuPrepareError::Admission(
        AlignmentGpuPageAdmissionError::Ledger(budget),
    ));
    assert_eq!(nested.persistent_budget_pressure(), Some(expected));
    let prepare = AlignmentGpuFrameBuildError::Prepare(AlignmentGpuPrepareError::Ledger(budget));
    assert_eq!(prepare.persistent_budget_pressure(), Some(expected));

    let transient = AlignmentGpuFrameBuildError::Prepare(AlignmentGpuPrepareError::Ledger(
        LedgerError::TransientBudgetExceeded {
            class: TransientResourceClass::GpuUpload,
            requested: 4_096,
            transient: 8_192,
            limit: 10_000,
        },
    ));
    assert_eq!(transient.persistent_budget_pressure(), None);
    assert_eq!(
        AlignmentGpuFrameBuildError::Prepare(AlignmentGpuPrepareError::InvalidFrameGeometry)
            .persistent_budget_pressure(),
        None
    );
}

#[test]
fn admitted_geometry_is_reused_across_camera_frames_and_device_discard_preserves_scene() {
    let Some((device, queue)) = headless_device() else {
        return;
    };
    let ledger = AllocationLedger::new_with_transient_budget(16 << 20, 2 << 20, 8 << 20);
    let scene = test_scene(&ledger);
    let semantic_handles = scene
        .pages()
        .iter()
        .flat_map(|page| page.primitives().iter().map(|primitive| primitive.handle))
        .collect::<Vec<_>>();
    let mut renderer = AlignmentGpuRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut expected_uploaded = 0_usize;
    for page in scene.pages() {
        match renderer
            .admit_page(&device, &queue, page, &ledger, 10)
            .unwrap()
        {
            AlignmentGpuPageAdmission::Admitted { uploaded_bytes, .. } => {
                expected_uploaded = expected_uploaded.saturating_add(uploaded_bytes);
            }
            AlignmentGpuPageAdmission::AlreadyResident => {
                panic!("fixture page must be admitted exactly once")
            }
            AlignmentGpuPageAdmission::NoGpuGeometry => {}
        }
    }
    let clip = rgv_alignment_scene::CpuClipRect {
        x: 0.0,
        y: 0.0,
        width: 256.0,
        height: 128.0,
    };
    let camera = AlignmentCamera::new(scene.prepared_interval(), 0.0, 12.0, 9.0, clip).unwrap();
    let interaction = AlignmentInteractionState::default();
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 1,
        camera,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let (first, first_stats) = renderer
        .prepare(&device, &queue, &[request], &ledger)
        .unwrap();
    assert_eq!(first_stats.geometry_uploaded_bytes, expected_uploaded);
    drop(first);

    let viewport = camera.viewport().pan_pixels(10.0, 100).unwrap();
    let panned = AlignmentCamera::from_viewport(viewport, 0.0, 12.0, 9.0, clip).unwrap();
    let request = AlignmentGpuSceneRequest {
        scene: &scene,
        camera_sequence: 2,
        camera: panned,
        screen_size_points: [256.0, 128.0],
        scissor: AlignmentGpuScissor {
            x: 0,
            y: 0,
            width: 256,
            height: 128,
        },
        interaction: &interaction,
    };
    let (second, second_stats) = renderer
        .prepare(&device, &queue, &[request], &ledger)
        .unwrap();
    assert_eq!(second_stats.geometry_uploaded_bytes, 0);
    assert_eq!(second_stats.page_cache_misses, 0);
    assert_eq!(second.scene_revisions.as_ref(), &[scene.revision()]);

    render_test_frame(&device, &queue, &renderer, &second).unwrap();
    renderer.discard_device_resources();
    assert_eq!(renderer.resident_page_count(), 0);
    assert!(matches!(
        render_test_frame(&device, &queue, &renderer, &second),
        Err(AlignmentGpuRenderError::DeviceGenerationMismatch { .. })
    ));
    assert!(
        semantic_handles
            .iter()
            .all(|handle| scene.contains_handle(*handle))
    );
    drop(second);
    assert_eq!(
        ledger
            .snapshot()
            .unwrap()
            .records
            .iter()
            .filter(|record| record.class == AllocationClass::GpuPage)
            .count(),
        0
    );
}
