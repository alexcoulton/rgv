//! Coordinator tests, grouped by the responsibility they exercise. Shared fixtures live here.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::{NonZeroU32, NonZeroU64},
};

use rgv_alignment_scene::{
    AlignmentSceneCamera, AlignmentSceneSnapshot, AlignmentViewFilterContract, CanonicalRowBlock,
    ContractFingerprint, LayoutContract, LedgerError, PayloadContract, PresentationContract,
    ReferenceRevision, SceneRevision, SummaryContract,
};
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::ReferenceSlice;

use crate::{AlignmentSourceActor, AlignmentSourceActorEvent, SourceIntentKind, SourceOwnerId};

use std::{num::NonZeroUsize, path::Path, sync::Arc};

use rgv_alignment_scene::{
    AlignmentCamera, AlignmentComponent, AlignmentFilterPredicate, AlignmentFilterScope,
    AlignmentPanelLaneCapture, AlignmentPanelLanePublication, AlignmentPanelSceneAdmission,
    AlignmentPanelSceneStore, AlignmentStyleContract, AlleleSummaryDetail, AllocationLedger,
    CapabilityRequirement, EvidenceStore, GeometryLod, RevisionFingerprint,
};
use rgv_core::{
    AlignmentCigarEventPredicate, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
    OverlappingMatePolicy, PanelId, ReadStackingMode, TrackId,
};
use rgv_hts::{
    AlignedBaseClassification, AlignmentOpenOptions, AlignmentSource, LocalAlignmentSource,
    QueryOptions, TemplateLengthBounds,
};

use super::*;
use crate::{LocalBamSourceControl, PositionedAlignmentBackend};

type TestCoordinator = AlignmentSceneCoordinator<PositionedAlignmentBackend>;

fn no_payload() -> PayloadContract {
    PayloadContract::new(
        CapabilityRequirement::NotRequired,
        CapabilityRequirement::NotRequired,
        [*b"XS"],
        None,
    )
}

fn test_coordinator_with_ledger(ledger: AllocationLedger) -> (TestCoordinator, AlignmentCamera) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
    let source = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
    let contig = ContigId::new(0);
    let contig_length = source.reference_dictionary().get(contig).unwrap().length();
    let backend = PositionedAlignmentBackend::new(
        source,
        QueryOptions {
            max_records: NonZeroUsize::new(1_000).unwrap(),
            ..QueryOptions::default()
        },
    )
    .unwrap();
    test_coordinator_over(
        backend,
        GenomicInterval::new(contig, 9_999, 10_001).unwrap(),
        contig_length,
        NonZeroU64::new(1).unwrap(),
        ledger,
    )
}

/// A single-lane coordinator over `backend` with a camera on `interval`. `tile_bases` sizes
/// both geometry and summary tiles; the Complete read limit is 1,000.
fn test_coordinator_over(
    backend: PositionedAlignmentBackend,
    interval: GenomicInterval,
    contig_length: u64,
    tile_bases: NonZeroU64,
    ledger: AllocationLedger,
) -> (TestCoordinator, AlignmentCamera) {
    let contig = interval.contig();
    let source_revision = backend.source_revision();
    let record_contract = backend.record_contract();
    let store = EvidenceStore::new(source_revision, record_contract, ledger);
    let actor = AlignmentSourceActor::new(
        backend,
        store,
        NonZeroUsize::new(4).unwrap(),
        NonZeroUsize::new(2).unwrap(),
    );
    let style = AlignmentStyleContract {
        show_modified_bases: false,
        ..AlignmentStyleContract::default()
    };
    let contract = AlignmentSceneBuildContract {
        source_revision,
        record_contract,
        contig,
        contig_length,
        complete_read_limit: NonZeroUsize::new(1_000).unwrap(),
        payload: no_payload(),
        view_filter: AlignmentViewFilterContract::default(),
        highlight_filter: AlignmentViewFilterContract::default(),
        summaries: SummaryContract {
            overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            allele_detail: AlleleSummaryDetail::None,
            collect_modifications: false,
        },
        layout: LayoutContract {
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
        },
        presentation: PresentationContract {
            geometry_lod: GeometryLod::Read,
            style,
            reference: None,
            annotation: None,
        },
        geometry_tile_bases: tile_bases,
        summary_tile_bases: tile_bases,
        row_block_rows: NonZeroU32::new(8).unwrap(),
        normalization_mapped_records: None,
        reference: None,
        annotation: None,
    };
    let policy = AlignmentScenePreparationPolicy::new(1.0, 1.0, 0.25, 4, 1).unwrap();
    let coordinator =
        AlignmentSceneCoordinator::new(actor, contract, policy, SourceOwnerId::new(7)).unwrap();
    let camera = AlignmentCamera::new(
        interval,
        0.0,
        12.0,
        9.0,
        rgv_alignment_scene::CpuClipRect {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 72.0,
        },
    )
    .unwrap();
    (coordinator, camera)
}

/// A coordinator over the multi-megabyte benchmark BAM with a 20 kb camera at `camera_start`
/// on its second contig, and a log of every block its decoder reads.
fn wide_coordinator_with_ledger(
    camera_start: u64,
    ledger: AllocationLedger,
) -> (
    TestCoordinator,
    AlignmentCamera,
    crate::local_bam_source::test_support::DecodeLog,
) {
    const CAMERA_BASES: u64 = 20_000;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
    let source =
        rgv_hts::ProductionAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
    let contig = ContigId::new(1);
    let contig_length = source.reference_dictionary().get(contig).unwrap().length();
    let log = crate::local_bam_source::test_support::DecodeLog::default();
    let backend = PositionedAlignmentBackend::with_decode_source(
        source,
        QueryOptions {
            max_records: NonZeroUsize::new(1_000).unwrap(),
            ..QueryOptions::default()
        },
        |source| log.wrap(source),
    )
    .unwrap();
    let (coordinator, camera) = test_coordinator_over(
        backend,
        GenomicInterval::new(contig, camera_start, camera_start + CAMERA_BASES).unwrap(),
        contig_length,
        NonZeroU64::new(4_096).unwrap(),
        ledger,
    );
    (coordinator, camera, log)
}

fn wide_coordinator(
    camera_start: u64,
) -> (
    TestCoordinator,
    AlignmentCamera,
    crate::local_bam_source::test_support::DecodeLog,
) {
    wide_coordinator_with_ledger(
        camera_start,
        AllocationLedger::new_with_transient_budget(512 << 20, 64 << 20, 256 << 20),
    )
}

fn test_coordinator() -> (TestCoordinator, AlignmentCamera) {
    test_coordinator_with_ledger(AllocationLedger::new_with_transient_budget(
        64 << 20,
        32 << 20,
        64 << 20,
    ))
}

fn test_source_coordinator() -> (
    AlignmentSceneSourceCoordinator<PositionedAlignmentBackend>,
    AlignmentSceneBuildContract,
    AlignmentScenePreparationPolicy,
    AlignmentCamera,
) {
    test_source_coordinator_with_ledger(AllocationLedger::new_with_transient_budget(
        64 << 20,
        32 << 20,
        64 << 20,
    ))
}

fn test_source_coordinator_with_ledger(
    ledger: AllocationLedger,
) -> (
    AlignmentSceneSourceCoordinator<PositionedAlignmentBackend>,
    AlignmentSceneBuildContract,
    AlignmentScenePreparationPolicy,
    AlignmentCamera,
) {
    let (coordinator, camera) = test_coordinator_with_ledger(ledger);
    let contract = coordinator.contract().clone();
    let policy = coordinator.preparation_policy();
    let AlignmentSceneCoordinator { mut source, owner } = coordinator;
    assert!(source.remove_lane(owner));
    let AlignmentSceneSourceCoordinator { actor, .. } = source;
    (
        AlignmentSceneSourceCoordinator::new(actor),
        contract,
        policy,
        camera,
    )
}

fn test_coordinator_with_contract(
    configure: impl FnOnce(&mut AlignmentSceneBuildContract),
) -> (TestCoordinator, AlignmentCamera) {
    let (source, mut contract, policy, camera) = test_source_coordinator();
    configure(&mut contract);
    let AlignmentSceneSourceCoordinator { actor, .. } = source;
    let coordinator =
        AlignmentSceneCoordinator::new(actor, contract, policy, SourceOwnerId::new(7)).unwrap();
    (coordinator, camera)
}

fn test_reference_revision(value: u64) -> ReferenceRevision {
    ReferenceRevision::new(RevisionFingerprint::new(0x5245_4645_5245_4e43, value))
}

fn base_reference_coordinator(revision: ReferenceRevision) -> (TestCoordinator, AlignmentCamera) {
    test_coordinator_with_contract(|contract| {
        contract.payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"XS"],
            None,
        );
        contract.presentation.geometry_lod = GeometryLod::Base;
        contract.presentation.reference = Some(revision);
        contract.reference = None;
    })
}

fn allele_reference_coordinator(revision: ReferenceRevision) -> (TestCoordinator, AlignmentCamera) {
    test_coordinator_with_contract(|contract| {
        contract.payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"XS"],
            None,
        );
        contract.summaries.allele_detail = AlleleSummaryDetail::Counts;
        contract.presentation.reference = Some(revision);
        contract.reference = None;
    })
}

fn reference_authority(
    revision: ReferenceRevision,
    interval: GenomicInterval,
) -> AlignmentReferenceAuthority {
    let bases = vec![b'A'; usize::try_from(interval.len()).unwrap()];
    AlignmentReferenceAuthority::new(revision, Arc::new(ReferenceSlice { interval, bases }))
}

fn pump_until_source_complete_with_core_publication(coordinator: &mut TestCoordinator) {
    for _ in 0..256 {
        let event = coordinator.pump_once().unwrap();
        if matches!(
            event.source_event,
            Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
        ) {
            let frame = coordinator.frame().unwrap();
            assert!(frame.visible_complete());
            assert!(frame.scene_contracts_current);
            assert!(!frame.reference_products_current);
            assert_eq!(
                frame
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .reads()
                    .presentation()
                    .reference,
                None
            );
            return;
        }
    }
    panic!("bounded local fixture did not complete source acquisition");
}

fn pump_until_publication(coordinator: &mut TestCoordinator) -> SceneRevision {
    for _ in 0..256 {
        let event = coordinator.pump_once().unwrap();
        if let Some(revision) = event.published_scene {
            return revision;
        }
    }
    panic!("bounded local fixture did not publish a scene");
}

fn pump_until_speculation_complete(coordinator: &mut TestCoordinator) {
    for _ in 0..256 {
        let event = coordinator.pump_once().unwrap();
        if matches!(
            event.source_event,
            Some(AlignmentSourceActorEvent::AcquisitionCompleted { publish: None, .. })
        ) {
            return;
        }
    }
    panic!("bounded local fixture did not complete speculation");
}

fn scene_population(scene: &AlignmentSceneSnapshot) -> Vec<rgv_alignment_scene::AlignmentHandle> {
    let mut handles = scene
        .reads()
        .evidence()
        .select(scene.prepared_interval())
        .unwrap()
        .records()
        .iter()
        .map(|record| record.handle)
        .collect::<Vec<_>>();
    handles.sort_unstable();
    handles
}

fn scrolled_camera(camera: AlignmentCamera, row_offset: f64) -> AlignmentCamera {
    AlignmentCamera::from_viewport(
        camera.viewport(),
        row_offset,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap()
}

/// One published lane whose row blocks are a single row tall, so a vertical scroll always
/// needs a new foreground row block and nothing else.
fn published_single_row_block_lane() -> (
    AlignmentSceneSourceCoordinator<PositionedAlignmentBackend>,
    SourceOwnerId,
    AlignmentCamera,
) {
    let (mut coordinator, mut contract, policy, camera) = test_source_coordinator_with_ledger(
        AllocationLedger::new_with_transient_budget(64 << 20, 0, 64 << 20),
    );
    contract.row_block_rows = NonZeroU32::new(1).unwrap();
    let owner = SourceOwnerId::new(7);
    coordinator.insert_lane(owner, contract, policy).unwrap();
    coordinator.update_camera(owner, camera).unwrap();
    assert!(
        (0..256).any(|_| !coordinator.pump_once().unwrap().publications.is_empty()),
        "the fixture lane should publish its first scene"
    );
    (coordinator, owner, camera)
}

/// Everything observable about a published scene which a staged build must reproduce.
#[derive(Debug, PartialEq)]
struct SceneSignature {
    prepared_interval: GenomicInterval,
    prepared_rows: std::ops::Range<u32>,
    population: Vec<rgv_alignment_scene::AlignmentHandle>,
    row_count: usize,
    read_pages: usize,
    summary_pages: usize,
    visible_complete: bool,
    evidence_candidates: u64,
    exact_summary_candidates: u64,
    row_atlas_candidates: u64,
    charged: usize,
    reserved: usize,
}

fn scene_signature(coordinator: &TestCoordinator) -> SceneSignature {
    let frame = coordinator.frame().unwrap();
    let scene = frame.snapshot.as_ref().expect("a published scene");
    let telemetry = coordinator.telemetry();
    let progress = coordinator.actor().store().ledger().progress().unwrap();
    SceneSignature {
        prepared_interval: scene.prepared_interval(),
        prepared_rows: scene.reads().prepared_rows(),
        population: scene_population(scene),
        row_count: scene.reads().atlas().row_count(),
        read_pages: scene.reads().pages().len(),
        summary_pages: scene.summary_pages().len(),
        visible_complete: frame.visible_complete(),
        evidence_candidates: telemetry.evidence_candidates,
        exact_summary_candidates: telemetry.exact_summary_candidates,
        row_atlas_candidates: telemetry.row_atlas_candidates,
        charged: progress.charged,
        reserved: progress.reserved,
    }
}

fn camera_at(coordinator: &TestCoordinator, like: AlignmentCamera, start: u64) -> AlignmentCamera {
    AlignmentCamera::new(
        GenomicInterval::new(
            coordinator.contract().contig,
            start,
            start + like.interval().len(),
        )
        .unwrap(),
        0.0,
        like.row_height(),
        like.read_height(),
        like.clip(),
    )
    .unwrap()
}

/// Drives source and staged-build work to rest, returning the stages run in order.
fn pump_until_idle(coordinator: &mut TestCoordinator) -> Vec<&'static str> {
    let owner = coordinator.owner();
    let mut stages = Vec::new();
    for _ in 0..1_024 {
        if let Some(stage) = coordinator.source.scene_build_stage(owner) {
            stages.push(stage);
            coordinator.source.pump_scene_build_once().unwrap().unwrap();
        } else if coordinator.source.has_pending_source_work() {
            coordinator.pump_once().unwrap();
        } else {
            return stages;
        }
    }
    panic!("the bounded local fixture did not come to rest");
}

mod admission;
mod bounded;
mod camera;
mod capacity;
mod pressure;
mod reference;
mod row_activity;
mod scene_build;
mod stream_continuation;
mod vertical_residency;
