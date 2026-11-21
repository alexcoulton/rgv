use std::{collections::BTreeMap, sync::Arc};

use rgv_core::{PanelId, SashimiCountMode, TrackId, Viewport};

use crate::{
    AlignmentCoherenceGroupId, AlignmentCoherenceGroupKind, AlignmentCoverageLaneOptions,
    AlignmentDerivedLaneCapture, AlignmentDerivedLaneKind, AlignmentDerivedLaneOptions,
    AlignmentModificationLaneOptions, AlignmentPanelLaneFrame, AlignmentPanelSceneFrame,
    AlignmentSashimiLaneOptions, AlignmentSashimiScalePolicy, AlignmentSceneSnapshot,
    AlleleSummaryDetail, CpuSummaryCamera, CpuSummaryFrame, CpuSummaryProjectionError,
    CpuSummaryProjectionOptions, CpuSummaryProjector,
};

/// Disposable CPU projections for every captured derived lane in one immutable panel epoch.
///
/// The identifiers and viewport are copied from the input scene frame. No source, mutable session
/// setting, or legacy query is consulted while building this value.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentPanelSummaryFrame {
    pub panel_id: PanelId,
    pub viewport: Viewport,
    pub camera_revision: u64,
    pub authority_revision: u64,
    pub lanes: Arc<[AlignmentPanelDerivedLaneFrame]>,
}

/// One captured lane and its explicit projection state. Retaining the capture keeps clip geometry
/// and immutable presentation policy beside the resulting CPU frame, including modification
/// colour/selection metadata that does not belong in the low-level semantic projector.
#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentPanelDerivedLaneFrame {
    pub track_id: TrackId,
    pub capture: AlignmentDerivedLaneCapture,
    pub readiness: AlignmentPanelDerivedLaneReadiness,
}

impl AlignmentPanelDerivedLaneFrame {
    #[must_use]
    pub const fn kind(&self) -> AlignmentDerivedLaneKind {
        self.capture.kind()
    }
}

/// A pending lane may become ready when the current scene/group authority arrives. Unavailable
/// means that the captured policy cannot be represented by the immutable authority in this epoch.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AlignmentPanelDerivedLaneReadiness {
    Pending,
    Unavailable(AlignmentPanelDerivedLaneUnavailable),
    Ready(Box<AlignmentPanelDerivedLaneSummary>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AlignmentPanelDerivedLaneUnavailable {
    ReferenceBackedAlleles,
    ModificationSummary,
    MissingSharedSashimiGroup(AlignmentCoherenceGroupId),
    InvalidSharedSashimiGroup(AlignmentCoherenceGroupId),
    IncompatibleSharedSashimiCountMode(AlignmentCoherenceGroupId),
    SashimiNormalization,
}

/// Typed CPU summary frames for one ready lane. The enum is intentionally extensible for future
/// scene-owned coverage selection and bisulfite interpretation policies.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AlignmentPanelDerivedLaneSummary {
    Coverage {
        depth: CpuSummaryFrame,
        alleles: Option<CpuSummaryFrame>,
    },
    Sashimi {
        junctions: CpuSummaryFrame,
    },
    Modifications {
        modifications: CpuSummaryFrame,
    },
}

#[derive(Debug, Default)]
pub struct AlignmentPanelSummaryProjector;

impl AlignmentPanelSummaryProjector {
    /// Projects all derived lanes from exactly one captured panel scene epoch. Projection failure
    /// aborts the whole result, so a caller cannot publish a partially projected shared group.
    pub fn project(
        panel: &AlignmentPanelSceneFrame,
    ) -> Result<AlignmentPanelSummaryFrame, CpuSummaryProjectionError> {
        let shared = resolve_shared_sashimi_scales(panel)?;
        let lane_count = panel
            .lanes
            .iter()
            .map(|lane| lane.derived_lanes.len())
            .sum();
        let mut lanes = Vec::with_capacity(lane_count);
        for lane in panel.lanes.iter() {
            for capture in lane.derived_lanes.iter() {
                lanes.push(AlignmentPanelDerivedLaneFrame {
                    track_id: lane.track_id,
                    capture: capture.clone(),
                    readiness: project_lane(panel, lane, capture, &shared)?,
                });
            }
        }
        Ok(AlignmentPanelSummaryFrame {
            panel_id: panel.panel_id,
            viewport: panel.viewport,
            camera_revision: panel.camera_revision,
            authority_revision: panel.authority_revision,
            lanes: lanes.into(),
        })
    }
}

fn project_lane(
    panel: &AlignmentPanelSceneFrame,
    lane: &AlignmentPanelLaneFrame,
    capture: &AlignmentDerivedLaneCapture,
    shared: &BTreeMap<AlignmentCoherenceGroupId, SharedSashimiResolution>,
) -> Result<AlignmentPanelDerivedLaneReadiness, CpuSummaryProjectionError> {
    if !lane.summary_current() {
        return Ok(AlignmentPanelDerivedLaneReadiness::Pending);
    }
    let Some(scene) = lane.snapshot.as_ref() else {
        return Ok(AlignmentPanelDerivedLaneReadiness::Pending);
    };
    let camera = CpuSummaryCamera::from_viewport(lane.preparation_camera.viewport(), capture.clip)?;
    match &capture.options {
        AlignmentDerivedLaneOptions::Coverage(options) => {
            project_coverage(panel.camera_revision, scene, camera, *options)
        }
        AlignmentDerivedLaneOptions::Sashimi(options) => {
            project_sashimi(panel.camera_revision, lane, scene, camera, *options, shared)
        }
        AlignmentDerivedLaneOptions::Modifications(options) => {
            project_modifications(panel.camera_revision, scene, camera, options)
        }
    }
}

fn project_coverage(
    camera_revision: u64,
    scene: &Arc<AlignmentSceneSnapshot>,
    camera: CpuSummaryCamera,
    options: AlignmentCoverageLaneOptions,
) -> Result<AlignmentPanelDerivedLaneReadiness, CpuSummaryProjectionError> {
    let depth = CpuSummaryProjector::project(
        scene,
        camera_revision,
        camera,
        &CpuSummaryProjectionOptions::Depth(options.depth),
    )?;
    let alleles = options
        .alleles
        .filter(|_| {
            scene.reads().presentation().reference.is_some()
                && scene.summaries().contract().allele_detail != AlleleSummaryDetail::None
        })
        .map(|options| {
            CpuSummaryProjector::project(
                scene,
                camera_revision,
                camera,
                &CpuSummaryProjectionOptions::Alleles(options),
            )
        })
        .transpose()?;
    Ok(AlignmentPanelDerivedLaneReadiness::Ready(Box::new(
        AlignmentPanelDerivedLaneSummary::Coverage { depth, alleles },
    )))
}

fn project_sashimi(
    camera_revision: u64,
    lane: &AlignmentPanelLaneFrame,
    scene: &Arc<AlignmentSceneSnapshot>,
    camera: CpuSummaryCamera,
    options: AlignmentSashimiLaneOptions,
    shared: &BTreeMap<AlignmentCoherenceGroupId, SharedSashimiResolution>,
) -> Result<AlignmentPanelDerivedLaneReadiness, CpuSummaryProjectionError> {
    let (scale_maximum, normalization_checked) = match options.scale {
        AlignmentSashimiScalePolicy::Automatic => (None, false),
        AlignmentSashimiScalePolicy::Fixed(maximum) => (Some(maximum), false),
        AlignmentSashimiScalePolicy::Shared(group_id) => {
            let resolution =
                shared
                    .get(&group_id)
                    .copied()
                    .unwrap_or(SharedSashimiResolution::Unavailable(
                        AlignmentPanelDerivedLaneUnavailable::MissingSharedSashimiGroup(group_id),
                    ));
            match resolution {
                SharedSashimiResolution::Pending => {
                    return Ok(AlignmentPanelDerivedLaneReadiness::Pending);
                }
                SharedSashimiResolution::Unavailable(reason) => {
                    return Ok(AlignmentPanelDerivedLaneReadiness::Unavailable(reason));
                }
                SharedSashimiResolution::Ready { maximum } => (Some(maximum), true),
            }
        }
    };
    if options.projection.count_mode == SashimiCountMode::CountsPerMillion && !normalization_checked
    {
        let measurement = CpuSummaryProjector::measure_junction_scale(
            scene,
            lane.preparation_camera.interval(),
            options.projection,
        )?;
        if !measurement.normalization_available {
            return Ok(AlignmentPanelDerivedLaneReadiness::Unavailable(
                AlignmentPanelDerivedLaneUnavailable::SashimiNormalization,
            ));
        }
    }
    let mut projection = options.projection;
    projection.scale_maximum = scale_maximum;
    let junctions = CpuSummaryProjector::project(
        scene,
        camera_revision,
        camera,
        &CpuSummaryProjectionOptions::Junctions(projection),
    )?;
    Ok(AlignmentPanelDerivedLaneReadiness::Ready(Box::new(
        AlignmentPanelDerivedLaneSummary::Sashimi { junctions },
    )))
}

fn project_modifications(
    camera_revision: u64,
    scene: &Arc<AlignmentSceneSnapshot>,
    camera: CpuSummaryCamera,
    options: &AlignmentModificationLaneOptions,
) -> Result<AlignmentPanelDerivedLaneReadiness, CpuSummaryProjectionError> {
    if !scene.summaries().contract().collect_modifications {
        return Ok(AlignmentPanelDerivedLaneReadiness::Unavailable(
            AlignmentPanelDerivedLaneUnavailable::ModificationSummary,
        ));
    }
    let modifications = CpuSummaryProjector::project_dominant_modifications(
        scene,
        camera_revision,
        camera,
        &options.projection,
    )?;
    Ok(AlignmentPanelDerivedLaneReadiness::Ready(Box::new(
        AlignmentPanelDerivedLaneSummary::Modifications { modifications },
    )))
}

#[derive(Clone, Copy, Debug)]
enum SharedSashimiResolution {
    Pending,
    Unavailable(AlignmentPanelDerivedLaneUnavailable),
    Ready { maximum: f64 },
}

#[derive(Clone, Copy)]
struct SharedSashimiParticipant<'a> {
    track_id: TrackId,
    lane: &'a AlignmentPanelLaneFrame,
    options: AlignmentSashimiLaneOptions,
}

fn resolve_shared_sashimi_scales(
    panel: &AlignmentPanelSceneFrame,
) -> Result<BTreeMap<AlignmentCoherenceGroupId, SharedSashimiResolution>, CpuSummaryProjectionError>
{
    let mut participants =
        BTreeMap::<AlignmentCoherenceGroupId, Vec<SharedSashimiParticipant<'_>>>::new();
    for lane in panel.lanes.iter() {
        for capture in lane.derived_lanes.iter() {
            let AlignmentDerivedLaneOptions::Sashimi(options) = capture.options else {
                continue;
            };
            let AlignmentSashimiScalePolicy::Shared(group_id) = options.scale else {
                continue;
            };
            participants
                .entry(group_id)
                .or_default()
                .push(SharedSashimiParticipant {
                    track_id: lane.track_id,
                    lane,
                    options,
                });
        }
    }

    let mut resolutions = BTreeMap::new();
    for (group_id, members) in participants {
        resolutions.insert(
            group_id,
            resolve_shared_sashimi_scale(panel, group_id, &members)?,
        );
    }
    Ok(resolutions)
}

fn resolve_shared_sashimi_scale(
    panel: &AlignmentPanelSceneFrame,
    group_id: AlignmentCoherenceGroupId,
    members: &[SharedSashimiParticipant<'_>],
) -> Result<SharedSashimiResolution, CpuSummaryProjectionError> {
    let Some(group) = panel
        .coherence_groups
        .iter()
        .find(|group| group.id == group_id)
    else {
        return Ok(SharedSashimiResolution::Unavailable(
            AlignmentPanelDerivedLaneUnavailable::MissingSharedSashimiGroup(group_id),
        ));
    };
    if group.kind != AlignmentCoherenceGroupKind::SashimiScale {
        return Ok(SharedSashimiResolution::Unavailable(
            AlignmentPanelDerivedLaneUnavailable::InvalidSharedSashimiGroup(group_id),
        ));
    }
    if !group.summary_complete {
        return Ok(SharedSashimiResolution::Pending);
    }
    let mut participant_ids = members
        .iter()
        .map(|participant| participant.track_id)
        .collect::<Vec<_>>();
    participant_ids.sort_unstable();
    participant_ids.dedup();
    if participant_ids.as_slice() != group.members.as_ref() {
        return Ok(SharedSashimiResolution::Unavailable(
            AlignmentPanelDerivedLaneUnavailable::InvalidSharedSashimiGroup(group_id),
        ));
    }
    let count_mode = members[0].options.projection.count_mode;
    if members
        .iter()
        .any(|member| member.options.projection.count_mode != count_mode)
    {
        return Ok(SharedSashimiResolution::Unavailable(
            AlignmentPanelDerivedLaneUnavailable::IncompatibleSharedSashimiCountMode(group_id),
        ));
    }

    let mut maximum = f64::EPSILON;
    for member in members {
        let Some(scene) = member.lane.snapshot.as_ref() else {
            return Ok(SharedSashimiResolution::Unavailable(
                AlignmentPanelDerivedLaneUnavailable::InvalidSharedSashimiGroup(group_id),
            ));
        };
        let measurement = CpuSummaryProjector::measure_junction_scale(
            scene,
            member.lane.preparation_camera.interval(),
            member.options.projection,
        )?;
        if !measurement.normalization_available {
            return Ok(SharedSashimiResolution::Unavailable(
                AlignmentPanelDerivedLaneUnavailable::SashimiNormalization,
            ));
        }
        maximum = maximum.max(measurement.maximum);
    }
    Ok(SharedSashimiResolution::Ready { maximum })
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, num::NonZeroU64};

    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId,
        CoverageScaleTransform, GenomicInterval, JunctionDisplayMode, ModifiedBaseColorMode,
        OverlappingMatePolicy, ReadStackingMode,
    };
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, ReferenceSlice,
        SequenceData, TemplateLengthBounds,
    };

    use super::*;
    use crate::{
        AlignmentCoherenceGroupFrame, AlignmentCoverageLaneOptions,
        AlignmentModificationLaneOptions, AlignmentPanelLaneFrame, AlignmentSceneCamera,
        AlignmentSourceRevision, AllocationClass, AllocationLedger, CanonicalGenomicTile,
        CanonicalRecordChunk, CapabilityRequirement, CompleteCoverage, ContractFingerprint,
        CpuAlleleProjectionOptions, CpuClipRect, CpuDepthProjectionOptions,
        CpuJunctionProjectionOptions, CpuModificationProjectionOptions,
        CpuSummaryProjectedComponent, EvidenceAcquisition, EvidenceChunk, EvidenceRevision,
        EvidenceRootBuilder, ExactAlleleSummaryTile, ExactDepthSummaryTile, ExactFilterSummaryTile,
        ExactJunctionSummaryTile, ExactModificationSummaryTile, ExactPairSummaryTile,
        ExactSummaryRoot, FilteredRecordProvenance, GeometryLod, LayoutContract, PayloadContract,
        PayloadSidecar, PresentationContract, ReferenceRevision, ReservationKind,
        RevisionFingerprint, RowAtlasRevision, SourceChunkId, SourceFingerprint, SummaryContract,
        SummaryPageFamily, SummaryRevision,
    };

    const RECORD_CONTRACT: ContractFingerprint = ContractFingerprint::new(91);

    fn source_revision(seed: u64) -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(seed, seed + 1)),
            RevisionFingerprint::new(seed + 2, seed + 3),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn snapshot(
        seed: u64,
        record_count: usize,
        normalization: Option<u64>,
    ) -> Arc<crate::AlignmentSceneSnapshot> {
        let ledger = AllocationLedger::new(64 << 20, 0);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let source_revision = source_revision(seed);
        let mut decoded = AlignmentBatch::default();
        for index in 0..record_count {
            let code = if index % 2 == 0 { 'm' } else { 'h' };
            let probability = if code == 'm' { 240 } else { 80 };
            let record = AlignmentRecord {
                contig: ContigId::new(0),
                start: 10,
                end: 23,
                flags: AlignmentFlags::from_bits(0),
                mapping_quality: Some(60),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: format!("spliced-{seed}-{index}").into_bytes(),
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
                    bases: b"CCCCCCCCCC".to_vec(),
                    qualities: Some(vec![35; 10]),
                },
                auxiliary_fields: vec![
                    AuxField {
                        tag: *b"MM",
                        value: AuxValue::String(format!("C+{code},0;")),
                    },
                    AuxField {
                        tag: *b"ML",
                        value: AuxValue::UnsignedArray(vec![probability]),
                    },
                ],
            };
            decoded
                .push_with_source_position(
                    &record,
                    AlignmentSourcePosition::bam_virtual_offset(
                        (5 << 16) | u64::try_from(index + 1).unwrap(),
                    ),
                )
                .unwrap();
        }
        let payload_contract = PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        );
        let payload =
            Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, None).unwrap());
        let core = CanonicalRecordChunk::new(
            source_revision,
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
            .register(payload_bytes, seed)
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
            .register(footprint.core_bytes, seed)
            .unwrap();
        let overlap_allocation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.overlap_index_bytes, seed)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let coverage = CompleteCoverage::new(
            source_revision,
            RECORD_CONTRACT,
            interval,
            FilteredRecordProvenance {
                records_seen: u64::try_from(record_count).unwrap(),
                eligible_records: u64::try_from(record_count).unwrap(),
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let acquisition = EvidenceAcquisition::new(coverage, [chunk]).unwrap();
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(seed + 4, seed + 5)),
            source_revision,
            RECORD_CONTRACT,
            payload_contract,
            1,
            1,
            &ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        let evidence = builder.build(seed + 6).unwrap();
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
            &ledger,
            ReservationKind::Normal,
            seed + 7,
        )
        .unwrap();
        let alleles = ExactAlleleSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.alleles(),
            &ledger,
            ReservationKind::Normal,
            seed + 7,
        )
        .unwrap();
        let junctions = ExactJunctionSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.junctions(),
            normalization,
            &ledger,
            ReservationKind::Normal,
            seed + 7,
        )
        .unwrap();
        let pairs = ExactPairSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.pairs(),
            &ledger,
            ReservationKind::Normal,
            seed + 7,
        )
        .unwrap();
        let filters = ExactFilterSummaryTile::build(
            &evidence,
            tile,
            100,
            interval,
            contract.filters(),
            &ledger,
            ReservationKind::Normal,
            seed + 7,
        )
        .unwrap();
        let modifications = ExactModificationSummaryTile::build(
            &evidence,
            tile,
            100,
            contract.modifications().unwrap(),
            &ledger,
            ReservationKind::Normal,
            seed + 7,
        )
        .unwrap();
        let summaries = ExactSummaryRoot::build(
            SummaryRevision::new(RevisionFingerprint::new(seed + 8, seed + 9)),
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
            &ledger,
            ReservationKind::Normal,
            seed + 10,
        )
        .unwrap();
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
            sampling: crate::ReadSampling::Complete,
        };
        let atlas = crate::AlignmentRowAtlas::build(
            RowAtlasRevision::new(RevisionFingerprint::new(seed + 11, seed + 12)),
            &evidence,
            interval,
            layout,
            &ledger,
            ReservationKind::Normal,
            seed + 13,
        )
        .unwrap();
        let reference_revision =
            ReferenceRevision::new(RevisionFingerprint::new(seed + 14, seed + 15));
        let reference = ReferenceSlice {
            interval,
            bases: vec![b'A'; usize::try_from(interval.len()).unwrap()],
        };
        let presentation = PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: crate::AlignmentStyleContract::default(),
            reference: Some(reference_revision),
            annotation: None,
        };
        let mut store = crate::AlignmentPresentationStore::new(ledger);
        let row_count = u32::try_from(atlas.row_count()).unwrap();
        let candidate = store
            .prepare(
                &evidence,
                &summaries,
                &atlas,
                presentation,
                interval,
                0..row_count,
                100,
                NonZeroU64::new(100).unwrap(),
                NonZeroU32::new(16).unwrap(),
                Some(&reference),
                ReservationKind::Normal,
            )
            .unwrap();
        store.stage(candidate)
    }

    fn clip(y: f64) -> CpuClipRect {
        CpuClipRect {
            x: 17.0,
            y,
            width: 500.0,
            height: 60.0,
        }
    }

    fn coverage_capture(y: f64, alleles: bool) -> AlignmentDerivedLaneCapture {
        AlignmentDerivedLaneCapture::new(
            clip(y),
            AlignmentDerivedLaneOptions::Coverage(AlignmentCoverageLaneOptions {
                depth: CpuDepthProjectionOptions {
                    show_center_line: false,
                    ..CpuDepthProjectionOptions::default()
                },
                alleles: alleles.then_some(CpuAlleleProjectionOptions {
                    minimum_fraction: 0.0,
                    quality_weighted: false,
                    scale_maximum: None,
                    transform: CoverageScaleTransform::Linear,
                    interpretation: crate::CoverageAlleleInterpretation::GenericReferenceAware,
                }),
            }),
        )
        .unwrap()
    }

    fn sashimi_capture(y: f64, group_id: AlignmentCoherenceGroupId) -> AlignmentDerivedLaneCapture {
        AlignmentDerivedLaneCapture::new(
            clip(y),
            AlignmentDerivedLaneOptions::Sashimi(AlignmentSashimiLaneOptions {
                projection: CpuJunctionProjectionOptions {
                    minimum_depth: 1,
                    minimum_flank: 0,
                    display_mode: JunctionDisplayMode::Collapsed,
                    scale_maximum: None,
                    count_mode: SashimiCountMode::CountsPerMillion,
                    show_labels: false,
                },
                scale: AlignmentSashimiScalePolicy::Shared(group_id),
            }),
        )
        .unwrap()
    }

    fn modification_capture(y: f64) -> AlignmentDerivedLaneCapture {
        AlignmentDerivedLaneCapture::new(
            clip(y),
            AlignmentDerivedLaneOptions::Modifications(AlignmentModificationLaneOptions {
                projection: CpuModificationProjectionOptions::default(),
                color_mode: ModifiedBaseColorMode::ByType,
                selected_position: Some(10),
            }),
        )
        .unwrap()
    }

    fn lane(
        track_id: TrackId,
        scene: Arc<crate::AlignmentSceneSnapshot>,
        viewport: Viewport,
        captures: impl IntoIterator<Item = AlignmentDerivedLaneCapture>,
    ) -> AlignmentPanelLaneFrame {
        AlignmentPanelLaneFrame {
            track_id,
            preparation_camera: AlignmentSceneCamera::without_visible_rows(viewport),
            read_camera: None,
            row_activity: None,
            derived_lanes: captures.into_iter().collect::<Vec<_>>().into(),
            source_camera_sequence: 3,
            snapshot: Some(scene),
            covered_interval: Some(viewport.interval()),
            requested_rows: 0..0,
            covered_rows: 0..0,
            scene_contracts_current: true,
        }
    }

    fn panel(
        viewport: Viewport,
        lanes: impl IntoIterator<Item = AlignmentPanelLaneFrame>,
        coherence_groups: impl IntoIterator<Item = AlignmentCoherenceGroupFrame>,
    ) -> AlignmentPanelSceneFrame {
        AlignmentPanelSceneFrame {
            panel_id: PanelId::new(7),
            viewport,
            camera_revision: 31,
            authority_revision: 41,
            lanes: lanes.into_iter().collect::<Vec<_>>().into(),
            coherence_groups: coherence_groups.into_iter().collect::<Vec<_>>().into(),
        }
    }

    fn viewport() -> Viewport {
        Viewport::new(
            GenomicInterval::new(ContigId::new(0), 5, 25).unwrap(),
            500.0,
        )
        .unwrap()
    }

    fn ready_summary(lane: &AlignmentPanelDerivedLaneFrame) -> &AlignmentPanelDerivedLaneSummary {
        let AlignmentPanelDerivedLaneReadiness::Ready(summary) = &lane.readiness else {
            panic!("expected ready derived lane, got {:?}", lane.readiness);
        };
        summary.as_ref()
    }

    #[test]
    fn collapsed_reads_do_not_block_current_derived_summaries() {
        let viewport = viewport();
        let scene = snapshot(100, 1, Some(1_000));
        let input = panel(
            viewport,
            [lane(
                TrackId::new(1),
                scene,
                viewport,
                [coverage_capture(10.0, false)],
            )],
            [],
        );
        assert!(input.lanes[0].read_camera.is_none());

        let projected = AlignmentPanelSummaryProjector::project(&input).unwrap();

        assert_eq!(projected.panel_id, input.panel_id);
        assert_eq!(projected.viewport, input.viewport);
        assert_eq!(projected.camera_revision, input.camera_revision);
        assert_eq!(projected.authority_revision, input.authority_revision);
        let AlignmentPanelDerivedLaneSummary::Coverage { depth, alleles } =
            ready_summary(&projected.lanes[0])
        else {
            panic!("expected coverage projection");
        };
        assert_eq!(depth.camera_sequence, input.camera_revision);
        assert_eq!(depth.family, SummaryPageFamily::Depth);
        assert!(alleles.is_none());
    }

    #[test]
    fn coverage_projects_depth_and_reference_backed_alleles_from_one_capture() {
        let viewport = viewport();
        let input = panel(
            viewport,
            [lane(
                TrackId::new(1),
                snapshot(200, 1, Some(1_000)),
                viewport,
                [coverage_capture(10.0, true)],
            )],
            [],
        );

        let projected = AlignmentPanelSummaryProjector::project(&input).unwrap();
        let AlignmentPanelDerivedLaneSummary::Coverage {
            depth,
            alleles: Some(alleles),
        } = ready_summary(&projected.lanes[0])
        else {
            panic!("expected depth and allele frames");
        };
        assert!(depth.primitives.iter().any(|primitive| matches!(
            primitive.component,
            CpuSummaryProjectedComponent::Depth { depth: 1, .. }
        )));
        assert_eq!(alleles.family, SummaryPageFamily::Alleles);
        assert!(alleles.primitives.iter().any(|primitive| matches!(
            primitive.component,
            CpuSummaryProjectedComponent::Allele {
                allele: crate::CpuAlleleToken::C,
                ..
            }
        )));
    }

    #[test]
    fn shared_sashimi_waits_atomically_then_uses_one_cpm_maximum() {
        let viewport = viewport();
        let group_id = AlignmentCoherenceGroupId::new(88);
        let tracks = [TrackId::new(1), TrackId::new(2)];
        let scenes = [snapshot(300, 2, Some(100)), snapshot(400, 5, Some(1_000))];
        let lanes = tracks
            .into_iter()
            .zip(scenes.iter().cloned())
            .enumerate()
            .map(|(index, (track_id, scene))| {
                lane(
                    track_id,
                    scene,
                    viewport,
                    [sashimi_capture(10.0 + index as f64 * 70.0, group_id)],
                )
            })
            .collect::<Vec<_>>();
        let coherence = |summary_complete| AlignmentCoherenceGroupFrame {
            id: group_id,
            kind: AlignmentCoherenceGroupKind::SashimiScale,
            authority_revision: 41,
            members: tracks.into(),
            scene_revisions: scenes
                .iter()
                .map(|scene| Some(scene.revision()))
                .collect::<Vec<_>>()
                .into(),
            summary_complete,
            reads_complete: true,
            visible_complete: summary_complete,
        };

        let pending = AlignmentPanelSummaryProjector::project(&panel(
            viewport,
            lanes.clone(),
            [coherence(false)],
        ))
        .unwrap();
        assert!(
            pending
                .lanes
                .iter()
                .all(|lane| matches!(lane.readiness, AlignmentPanelDerivedLaneReadiness::Pending))
        );

        let ready =
            AlignmentPanelSummaryProjector::project(&panel(viewport, lanes, [coherence(true)]))
                .unwrap();
        let maxima = ready
            .lanes
            .iter()
            .map(|lane| {
                let AlignmentPanelDerivedLaneSummary::Sashimi { junctions } = ready_summary(lane)
                else {
                    panic!("expected Sashimi projection");
                };
                assert!(junctions.normalization_available);
                junctions.scale_maximum.unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(maxima, vec![20_000.0, 20_000.0]);
    }

    #[test]
    fn semantic_junction_measurement_honors_filters_and_count_mode() {
        let scene = snapshot(450, 2, Some(100));
        let interval = viewport().interval();
        let options = CpuJunctionProjectionOptions {
            minimum_depth: 2,
            minimum_flank: 5,
            count_mode: SashimiCountMode::CountsPerMillion,
            ..CpuJunctionProjectionOptions::default()
        };

        let measured =
            CpuSummaryProjector::measure_junction_scale(&scene, interval, options).unwrap();
        assert!(measured.normalization_available);
        assert_eq!(measured.maximum, 20_000.0);

        let below_depth = CpuSummaryProjector::measure_junction_scale(
            &scene,
            interval,
            CpuJunctionProjectionOptions {
                minimum_depth: 3,
                ..options
            },
        )
        .unwrap();
        assert_eq!(below_depth.maximum, f64::EPSILON);

        let below_flank = CpuSummaryProjector::measure_junction_scale(
            &scene,
            interval,
            CpuJunctionProjectionOptions {
                minimum_flank: 6,
                ..options
            },
        )
        .unwrap();
        assert_eq!(below_flank.maximum, f64::EPSILON);
    }

    #[test]
    fn exact_fractional_preparation_camera_drives_summary_pixels() {
        let viewport = viewport().pan_pixels(0.75, 100).unwrap();
        let input = panel(
            viewport,
            [lane(
                TrackId::new(1),
                snapshot(500, 1, Some(1_000)),
                viewport,
                [coverage_capture(10.0, false)],
            )],
            [],
        );

        let projected = AlignmentPanelSummaryProjector::project(&input).unwrap();
        let AlignmentPanelDerivedLaneSummary::Coverage { depth, .. } =
            ready_summary(&projected.lanes[0])
        else {
            panic!("expected coverage projection");
        };
        let x = depth
            .primitives
            .iter()
            .filter_map(|primitive| {
                matches!(
                    primitive.component,
                    CpuSummaryProjectedComponent::Depth { depth: 1, .. }
                )
                .then_some(primitive.x)
            })
            .min_by(f64::total_cmp)
            .unwrap();
        let expected = clip(10.0).x + viewport.pixel_at_genomic(10);
        let integer = clip(10.0).x + 125.0;
        assert!((x - expected).abs() < 1.0e-9);
        assert!((x - integer).abs() > 0.5);
    }

    #[test]
    fn modification_lane_keeps_only_one_deterministic_series_per_bin() {
        let viewport = viewport();
        let input = panel(
            viewport,
            [lane(
                TrackId::new(1),
                snapshot(600, 2, Some(1_000)),
                viewport,
                [modification_capture(10.0)],
            )],
            [],
        );

        let projected = AlignmentPanelSummaryProjector::project(&input).unwrap();
        let AlignmentPanelDerivedLaneSummary::Modifications { modifications } =
            ready_summary(&projected.lanes[0])
        else {
            panic!("expected modification projection");
        };
        let mut bins = BTreeMap::new();
        for primitive in modifications.primitives.iter() {
            let CpuSummaryProjectedComponent::Modification { series, .. } = primitive.component
            else {
                continue;
            };
            assert!(
                bins.insert((primitive.x.to_bits(), primitive.width.to_bits()), series)
                    .is_none(),
                "more than one modification series survived in one visible bin"
            );
        }
        assert!(!bins.is_empty());
        assert!(
            bins.values()
                .all(|series| series.code == rgv_hts::ModificationCode::Letter(b'm'))
        );
    }

    #[test]
    fn missing_cpm_normalization_is_explicitly_unavailable_for_the_whole_shared_group() {
        let viewport = viewport();
        let group_id = AlignmentCoherenceGroupId::new(89);
        let tracks = [TrackId::new(1), TrackId::new(2)];
        let scenes = [snapshot(650, 1, Some(1_000)), snapshot(660, 1, None)];
        let lanes = tracks
            .into_iter()
            .zip(scenes.iter().cloned())
            .enumerate()
            .map(|(index, (track_id, scene))| {
                lane(
                    track_id,
                    scene,
                    viewport,
                    [sashimi_capture(10.0 + index as f64 * 70.0, group_id)],
                )
            })
            .collect::<Vec<_>>();
        let group = AlignmentCoherenceGroupFrame {
            id: group_id,
            kind: AlignmentCoherenceGroupKind::SashimiScale,
            authority_revision: 41,
            members: tracks.into(),
            scene_revisions: scenes
                .iter()
                .map(|scene| Some(scene.revision()))
                .collect::<Vec<_>>()
                .into(),
            summary_complete: true,
            reads_complete: true,
            visible_complete: true,
        };

        let projected =
            AlignmentPanelSummaryProjector::project(&panel(viewport, lanes, [group])).unwrap();

        assert!(projected.lanes.iter().all(|lane| matches!(
            lane.readiness,
            AlignmentPanelDerivedLaneReadiness::Unavailable(
                AlignmentPanelDerivedLaneUnavailable::SashimiNormalization
            )
        )));
    }

    #[test]
    fn projection_errors_abort_the_panel_result() {
        let viewport = viewport();
        let invalid = AlignmentDerivedLaneCapture::new(
            clip(10.0),
            AlignmentDerivedLaneOptions::Sashimi(AlignmentSashimiLaneOptions {
                projection: CpuJunctionProjectionOptions {
                    scale_maximum: None,
                    ..CpuJunctionProjectionOptions::default()
                },
                scale: AlignmentSashimiScalePolicy::Fixed(-1.0),
            }),
        )
        .unwrap();
        let input = panel(
            viewport,
            [lane(
                TrackId::new(1),
                snapshot(680, 1, Some(1_000)),
                viewport,
                [invalid],
            )],
            [],
        );

        assert_eq!(
            AlignmentPanelSummaryProjector::project(&input).unwrap_err(),
            CpuSummaryProjectionError::InvalidScale
        );
    }

    #[test]
    fn stale_summary_authority_retains_each_captured_lane_as_pending() {
        let viewport = viewport();
        let mut stale = lane(
            TrackId::new(1),
            snapshot(700, 1, Some(1_000)),
            viewport,
            [coverage_capture(10.0, false), modification_capture(80.0)],
        );
        stale.scene_contracts_current = false;
        let projected =
            AlignmentPanelSummaryProjector::project(&panel(viewport, [stale], std::iter::empty()))
                .unwrap();

        assert_eq!(projected.lanes.len(), 2);
        assert!(
            projected
                .lanes
                .iter()
                .all(|lane| matches!(lane.readiness, AlignmentPanelDerivedLaneReadiness::Pending))
        );
    }
}
