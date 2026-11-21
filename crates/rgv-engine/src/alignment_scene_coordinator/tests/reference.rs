use super::*;

#[test]
fn allele_reference_requirement_adds_an_exact_bounded_context_halo() {
    let revision = test_reference_revision(101);
    let (mut coordinator, camera) = allele_reference_coordinator(revision);
    coordinator.update_camera(camera).unwrap();
    let source_horizon = GenomicInterval::new(camera.interval().contig(), 9_997, 10_003).unwrap();
    let reference_horizon =
        GenomicInterval::new(camera.interval().contig(), 9_995, 10_005).unwrap();
    assert_eq!(
        coordinator.frame().unwrap().reference_requirement,
        AlignmentReferenceRequirement::Pending {
            revision,
            horizon: reference_horizon,
        }
    );

    pump_until_source_complete_with_core_publication(&mut coordinator);
    let before = coordinator.telemetry();
    assert_eq!(
        coordinator
            .install_reference_authority(reference_authority(revision, source_horizon))
            .unwrap(),
        AlignmentReferenceInstallOutcome::default(),
        "a source-only horizon must not masquerade as bisulfite context authority"
    );
    let installed = coordinator
        .install_reference_authority(reference_authority(revision, reference_horizon))
        .unwrap();
    assert!(installed.installed);
    assert!(installed.published_scene.is_some());
    assert_eq!(
        coordinator.frame().unwrap().reference_requirement,
        AlignmentReferenceRequirement::Ready {
            revision,
            horizon: reference_horizon,
        }
    );
    assert_eq!(
        coordinator.telemetry().source_acquisitions,
        before.source_acquisitions,
        "installing the FASTA halo must reuse completed source roots"
    );

    let contract = coordinator.contract();
    assert_eq!(
        required_reference_horizon(
            contract,
            GenomicInterval::new(contract.contig, 0, 3).unwrap()
        ),
        GenomicInterval::new(contract.contig, 0, 5).unwrap()
    );
    assert_eq!(
        required_reference_horizon(
            contract,
            GenomicInterval::new(
                contract.contig,
                contract.contig_length - 3,
                contract.contig_length,
            )
            .unwrap()
        ),
        GenomicInterval::new(
            contract.contig,
            contract.contig_length - 5,
            contract.contig_length,
        )
        .unwrap()
    );
}

#[test]
fn substitution_filter_expands_reference_to_full_eligible_read_spans_without_more_bam_io() {
    let revision = test_reference_revision(151);
    let (mut coordinator, camera) = test_coordinator_with_contract(|contract| {
        contract.payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"XS"],
            None,
        );
        contract.view_filter = AlignmentViewFilterContract::new(
            1,
            [AlignmentFilterPredicate::Substitution {
                scope: AlignmentFilterScope::Anywhere,
                observed_base: None,
                minimum_count: Some(1),
                maximum_count: None,
            }],
        );
        contract.presentation.reference = Some(revision);
    });
    coordinator.update_camera(camera).unwrap();
    let initial = match coordinator.frame().unwrap().reference_requirement {
        AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
        other => panic!("expected pending reference requirement, got {other:?}"),
    };

    for _ in 0..256 {
        let event = coordinator.pump_once().unwrap();
        if matches!(
            event.source_event,
            Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
        ) {
            break;
        }
    }
    let expanded = match coordinator.frame().unwrap().reference_requirement {
        AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
        other => panic!("expected expanded pending reference requirement, got {other:?}"),
    };
    assert!(expanded.contains_interval(initial));
    let before = coordinator.telemetry().source_acquisitions;

    let installed = coordinator
        .install_reference_authority(reference_authority(revision, expanded))
        .unwrap();
    assert!(installed.installed);
    assert!(installed.published_scene.is_some());
    assert_eq!(coordinator.telemetry().source_acquisitions, before);
    let frame = coordinator.frame().unwrap();
    let evidence = frame.snapshot.unwrap().reads().evidence().clone();
    assert_eq!(
        evidence.view_filter().fingerprint(),
        coordinator.contract().view_filter_evaluation_fingerprint()
    );
    assert_eq!(
        evidence.view_filter().matching_records(),
        evidence.record_count()
    );
}

#[test]
fn source_completion_reports_reference_dependency_settlement_without_publication() {
    let revision = test_reference_revision(152);
    let (mut source, mut contract, policy, camera) = test_source_coordinator();
    contract.payload = PayloadContract::new(
        CapabilityRequirement::Required,
        CapabilityRequirement::Required,
        [*b"XS"],
        None,
    );
    contract.view_filter = AlignmentViewFilterContract::new(
        1,
        [AlignmentFilterPredicate::Substitution {
            scope: AlignmentFilterScope::Anywhere,
            observed_base: None,
            minimum_count: Some(1),
            maximum_count: None,
        }],
    );
    contract.presentation.reference = Some(revision);
    let owner = SourceOwnerId::new(7);
    source.insert_lane(owner, contract, policy).unwrap();
    source.update_camera(owner, camera).unwrap();
    let initial = match source.frame(owner).unwrap().unwrap().reference_requirement {
        AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
        other => panic!("expected pending reference requirement, got {other:?}"),
    };
    assert!(
        source
            .install_reference_authority(owner, reference_authority(revision, initial))
            .unwrap()
            .installed
    );

    let completion = (0..256)
        .find_map(|_| {
            let pump = source.pump_once().unwrap();
            matches!(
                pump.source_event,
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
            )
            .then_some(pump)
        })
        .expect("bounded local fixture did not complete source acquisition");

    assert!(completion.publications.is_empty());
    assert_eq!(completion.source_settlements, vec![owner]);
    let frame = source.frame(owner).unwrap().unwrap();
    let expanded = match frame.reference_requirement {
        AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
        other => panic!("expected expanded pending reference requirement, got {other:?}"),
    };
    assert!(expanded.contains_interval(initial));
    assert_ne!(expanded, initial);
    assert_eq!(
        source.owner_work_state(owner).unwrap(),
        AlignmentSceneOwnerWorkState {
            source_pending: false,
            presentation_pending: false,
        }
    );
}

#[test]
fn reference_backed_filter_rebases_a_retained_wide_scene_to_the_live_camera() {
    let revision = test_reference_revision(153);
    let (mut coordinator, narrow_camera) = test_coordinator_with_contract(|contract| {
        contract.payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"XS"],
            None,
        );
    });
    let broad_camera = AlignmentCamera::new(
        GenomicInterval::new(narrow_camera.interval().contig(), 9_000, 11_000).unwrap(),
        0.0,
        narrow_camera.row_height(),
        narrow_camera.read_height(),
        narrow_camera.clip(),
    )
    .unwrap();
    coordinator.update_camera(broad_camera).unwrap();
    pump_until_publication(&mut coordinator);
    let broad_prepared = coordinator.desired_prepared_interval().unwrap();

    let zoom_update = coordinator.update_camera(narrow_camera).unwrap();
    assert!(zoom_update.reused_current_scene);
    assert_eq!(
        coordinator.desired_prepared_interval(),
        Some(broad_prepared),
        "an ordinary zoom may safely retain the already prepared broad scene"
    );
    let before = coordinator.telemetry();

    let mut filtered = coordinator.contract().clone();
    filtered.view_filter = AlignmentViewFilterContract::new(
        1,
        [AlignmentFilterPredicate::Substitution {
            scope: AlignmentFilterScope::Anywhere,
            observed_base: None,
            minimum_count: Some(1),
            maximum_count: None,
        }],
    );
    filtered.presentation.reference = Some(revision);
    filtered.reference = None;
    let update = coordinator.replace_contract(filtered).unwrap();

    let rebased_prepared = coordinator.desired_prepared_interval().unwrap();
    assert_eq!(
        rebased_prepared,
        GenomicInterval::new(narrow_camera.interval().contig(), 9_997, 10_003).unwrap()
    );
    assert!(rebased_prepared.len() < broad_prepared.len());
    assert!(update.foreground_submitted.is_none());
    let reference_horizon = match coordinator.frame().unwrap().reference_requirement {
        AlignmentReferenceRequirement::Pending {
            revision: pending_revision,
            horizon,
        } => {
            assert_eq!(pending_revision, revision);
            horizon
        }
        other => panic!("expected a bounded pending filter reference, got {other:?}"),
    };
    assert!(reference_horizon.contains_interval(rebased_prepared));
    assert!(reference_horizon.len() < broad_prepared.len());
    let after = coordinator.telemetry();
    assert_eq!(after.source_work_started, before.source_work_started);
    assert_eq!(after.source_acquisitions, before.source_acquisitions);
}

#[test]
fn enabling_per_base_summaries_rebases_a_retained_wide_scene() {
    let (mut coordinator, narrow_camera) = test_coordinator_with_contract(|contract| {
        contract.payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"XS"],
            None,
        );
    });
    let broad_camera = AlignmentCamera::new(
        GenomicInterval::new(narrow_camera.interval().contig(), 9_000, 11_000).unwrap(),
        0.0,
        narrow_camera.row_height(),
        narrow_camera.read_height(),
        narrow_camera.clip(),
    )
    .unwrap();
    coordinator.update_camera(broad_camera).unwrap();
    pump_until_publication(&mut coordinator);
    let broad_prepared = coordinator.desired_prepared_interval().unwrap();

    let zoom_update = coordinator.update_camera(narrow_camera).unwrap();
    assert!(zoom_update.reused_current_scene);
    assert_eq!(
        coordinator.desired_prepared_interval(),
        Some(broad_prepared)
    );
    let source_acquisitions = coordinator.telemetry().source_acquisitions;

    let mut detailed = coordinator.contract().clone();
    detailed.summaries.allele_detail = AlleleSummaryDetail::Counts;
    let update = coordinator.replace_contract(detailed).unwrap();
    let rebased = coordinator.desired_prepared_interval().unwrap();

    assert_eq!(
        rebased,
        GenomicInterval::new(narrow_camera.interval().contig(), 9_997, 10_003).unwrap()
    );
    assert!(rebased.len() < broad_prepared.len());
    if update.published_scene.is_none() {
        pump_until_publication(&mut coordinator);
    }
    let scene = coordinator.frame().unwrap().snapshot.unwrap();
    assert_eq!(
        scene.summaries().contract().allele_detail,
        AlleleSummaryDetail::Counts
    );
    assert_eq!(scene.prepared_interval(), rebased);
    assert_eq!(
        coordinator.telemetry().source_acquisitions,
        source_acquisitions,
        "rebasing fine summaries must reuse already cached BAM evidence"
    );
    assert!(
        coordinator.telemetry().return_cache_entries_demoted > 0,
        "off-buffer summary tiles from the retained broad contract must yield before source evidence"
    );
}

#[test]
fn compatible_reference_authority_survives_a_stacked_filter_contract_replacement() {
    let revision = test_reference_revision(152);
    let (mut coordinator, camera) = test_coordinator_with_contract(|contract| {
        contract.payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"XS"],
            None,
        );
        contract.view_filter = AlignmentViewFilterContract::new(
            1,
            [AlignmentFilterPredicate::Substitution {
                scope: AlignmentFilterScope::Anywhere,
                observed_base: None,
                minimum_count: Some(1),
                maximum_count: None,
            }],
        );
        contract.presentation.reference = Some(revision);
    });
    coordinator.update_camera(camera).unwrap();
    let mut source_completed = false;
    for _ in 0..256 {
        let event = coordinator.pump_once().unwrap();
        if matches!(
            event.source_event,
            Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
        ) {
            source_completed = true;
            break;
        }
    }
    assert!(source_completed);
    let expanded = match coordinator.frame().unwrap().reference_requirement {
        AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
        other => panic!("expected pending reference requirement, got {other:?}"),
    };
    assert!(
        coordinator
            .install_reference_authority(reference_authority(revision, expanded))
            .unwrap()
            .installed
    );
    let source_acquisitions = coordinator.telemetry().source_acquisitions;

    let mut stacked = coordinator.contract().clone();
    stacked.reference = None;
    stacked.view_filter = AlignmentViewFilterContract::new(
        1,
        [
            AlignmentFilterPredicate::Substitution {
                scope: AlignmentFilterScope::Anywhere,
                observed_base: None,
                minimum_count: Some(1),
                maximum_count: None,
            },
            AlignmentFilterPredicate::CigarEvent {
                event: AlignmentCigarEventPredicate::Indel,
                scope: AlignmentFilterScope::Anywhere,
                minimum_length: None,
                maximum_length: None,
            },
        ],
    );
    let stacked_fingerprint = stacked.view_filter_evaluation_fingerprint();
    let update = coordinator.replace_contract(stacked).unwrap();
    if update.published_scene.is_none() {
        pump_until_publication(&mut coordinator);
    }

    assert!(coordinator.contract().reference.is_some());
    assert!(matches!(
        coordinator.frame().unwrap().reference_requirement,
        AlignmentReferenceRequirement::Ready { revision: ready, .. } if ready == revision
    ));
    assert_eq!(
        coordinator
            .frame()
            .unwrap()
            .snapshot
            .unwrap()
            .reads()
            .evidence()
            .view_filter()
            .fingerprint(),
        stacked_fingerprint
    );
    assert_eq!(
        coordinator.telemetry().source_acquisitions,
        source_acquisitions,
        "stacking an intrinsic filter must not trigger another BAM acquisition"
    );
}

#[test]
fn intrinsic_filter_and_clear_rederive_atomically_without_more_source_io() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let source_acquisitions = coordinator.telemetry().source_acquisitions;
    let original = coordinator.contract().clone();

    let mut filtered = original.clone();
    filtered.view_filter = AlignmentViewFilterContract::new(
        1,
        [AlignmentFilterPredicate::MappingQuality {
            minimum: Some(61),
            maximum: None,
        }],
    );
    let filtered_fingerprint = filtered.view_filter.fingerprint();
    let update = coordinator.replace_contract(filtered).unwrap();
    if update.published_scene.is_none() {
        pump_until_publication(&mut coordinator);
    }
    let filtered_scene = coordinator.frame().unwrap().snapshot.unwrap();
    assert_eq!(
        filtered_scene
            .reads()
            .evidence()
            .view_filter()
            .fingerprint(),
        filtered_fingerprint
    );
    assert!(
        filtered_scene
            .reads()
            .evidence()
            .view_filter()
            .eligible_records()
            > 0
    );
    assert_eq!(
        filtered_scene
            .reads()
            .evidence()
            .view_filter()
            .matching_records(),
        0
    );
    assert_eq!(filtered_scene.reads().atlas().row_count(), 0);
    assert_eq!(
        filtered_scene
            .summaries()
            .maximum_depth(filtered_scene.prepared_interval())
            .unwrap(),
        0
    );
    assert_eq!(
        coordinator.telemetry().source_acquisitions,
        source_acquisitions
    );
    let filter_telemetry = coordinator.telemetry();
    assert!(filter_telemetry.view_filter_evaluations > 0);
    assert_eq!(
        filter_telemetry.last_view_filter_contract_fingerprint,
        filtered_fingerprint.get()
    );
    assert_eq!(
        filter_telemetry.last_view_filter_evaluation_fingerprint,
        filtered_fingerprint.get()
    );
    assert!(filter_telemetry.view_filter_records_examined > 0);
    assert_eq!(filter_telemetry.view_filter_records_matched, 0);

    let update = coordinator.replace_contract(original).unwrap();
    if update.published_scene.is_none() {
        pump_until_publication(&mut coordinator);
    }
    let restored = coordinator.frame().unwrap().snapshot.unwrap();
    assert_eq!(
        restored.reads().evidence().view_filter().fingerprint(),
        ContractFingerprint::new(0)
    );
    assert_eq!(
        coordinator.telemetry().source_acquisitions,
        source_acquisitions
    );
    assert_eq!(
        coordinator.telemetry().view_filter_evaluations,
        filter_telemetry.view_filter_evaluations,
        "clearing the filter restores broad evidence without another predicate scan"
    );
}

#[test]
fn core_scene_completes_while_reference_is_pending_and_upgrade_reuses_source_roots() {
    let revision = test_reference_revision(2);
    let (mut coordinator, camera) = base_reference_coordinator(revision);
    coordinator.update_camera(camera).unwrap();
    let horizon = match coordinator.frame().unwrap().reference_requirement {
        AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
        other => panic!("expected pending reference requirement, got {other:?}"),
    };

    pump_until_source_complete_with_core_publication(&mut coordinator);
    assert!(coordinator.actor().store().current().is_some());
    assert!(coordinator.actor().store().current_summaries().is_some());
    assert!(coordinator.actor().store().current_row_atlas().is_some());
    let pending = coordinator.frame().unwrap();
    assert!(pending.snapshot.is_some());
    assert!(pending.scene_contracts_current);
    assert!(pending.visible_complete());
    assert!(!pending.reference_products_current);
    assert_eq!(
        pending.reference_requirement,
        AlignmentReferenceRequirement::Pending { revision, horizon }
    );
    let before = coordinator.telemetry();
    assert_eq!(before.source_acquisitions, 1);
    assert_eq!(before.presentation_candidates, 1);

    let installed = coordinator
        .install_reference_authority(reference_authority(revision, horizon))
        .unwrap();
    assert!(installed.installed);
    assert!(installed.published_scene.is_some());
    let after = coordinator.telemetry();
    assert_eq!(after.source_acquisitions, before.source_acquisitions);
    assert_eq!(after.source_work_started, before.source_work_started);
    assert_eq!(after.evidence_candidates, before.evidence_candidates);
    assert_eq!(
        after.exact_summary_candidates,
        before.exact_summary_candidates
    );
    assert_eq!(after.row_atlas_candidates, before.row_atlas_candidates);
    assert_eq!(
        after.presentation_candidates,
        before.presentation_candidates + 1
    );

    let ready = coordinator.frame().unwrap();
    assert_eq!(
        ready.reference_requirement,
        AlignmentReferenceRequirement::Ready { revision, horizon }
    );
    assert!(ready.scene_contracts_current);
    assert!(ready.reference_products_current);
    assert!(ready.visible_complete());
    assert_eq!(
        ready
            .snapshot
            .as_ref()
            .unwrap()
            .reads()
            .presentation()
            .reference,
        Some(revision)
    );
    let substitutions = ready
        .snapshot
        .as_ref()
        .unwrap()
        .reads()
        .pages()
        .iter()
        .flat_map(|page| page.primitives())
        .filter(|primitive| {
            matches!(
                primitive.component,
                AlignmentComponent::Base {
                    classification: AlignedBaseClassification::Substitution
                        | AlignedBaseClassification::DeclaredSubstitution,
                    ..
                }
            )
        })
        .count();
    assert!(substitutions > 0);
}

#[test]
fn stale_or_insufficient_reference_authority_is_an_owner_local_no_op() {
    let revision = test_reference_revision(3);
    let (mut coordinator, camera) = base_reference_coordinator(revision);
    coordinator.update_camera(camera).unwrap();
    let horizon = match coordinator.frame().unwrap().reference_requirement {
        AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
        other => panic!("expected pending reference requirement, got {other:?}"),
    };
    pump_until_source_complete_with_core_publication(&mut coordinator);
    let before = coordinator.telemetry();

    let stale = coordinator
        .install_reference_authority(reference_authority(test_reference_revision(300), horizon))
        .unwrap();
    assert_eq!(stale, AlignmentReferenceInstallOutcome::default());

    let short = GenomicInterval::new(
        horizon.contig(),
        horizon.start(),
        horizon.end().saturating_sub(1),
    )
    .unwrap();
    let insufficient = coordinator
        .install_reference_authority(reference_authority(revision, short))
        .unwrap();
    assert_eq!(insufficient, AlignmentReferenceInstallOutcome::default());
    assert!(coordinator.contract().reference.is_none());
    assert_eq!(coordinator.telemetry(), before);
    assert_eq!(
        coordinator.frame().unwrap().reference_requirement,
        AlignmentReferenceRequirement::Pending { revision, horizon }
    );
}

#[test]
fn installing_reference_for_one_lane_does_not_authorize_another_lane() {
    let (mut source, mut contract, policy, camera) = test_source_coordinator();
    let revision = test_reference_revision(5);
    contract.payload = PayloadContract::new(
        CapabilityRequirement::Required,
        CapabilityRequirement::Required,
        [*b"XS"],
        None,
    );
    contract.presentation.geometry_lod = GeometryLod::Base;
    contract.presentation.reference = Some(revision);
    contract.reference = None;
    let first = SourceOwnerId::new(41);
    let second = SourceOwnerId::new(42);
    source.insert_lane(first, contract.clone(), policy).unwrap();
    source.insert_lane(second, contract, policy).unwrap();

    source.update_camera(first, camera).unwrap();
    for _ in 0..256 {
        let pump = source.pump_once().unwrap();
        if matches!(
            pump.source_event,
            Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
        ) {
            break;
        }
    }
    source.update_camera(second, camera).unwrap();
    let horizon = match source.frame(first).unwrap().unwrap().reference_requirement {
        AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
        other => panic!("expected pending reference requirement, got {other:?}"),
    };

    let installed = source
        .install_reference_authority(first, reference_authority(revision, horizon))
        .unwrap();
    assert!(installed.installed);
    assert!(installed.published_scene.is_some());
    assert_eq!(
        source.frame(first).unwrap().unwrap().reference_requirement,
        AlignmentReferenceRequirement::Ready { revision, horizon }
    );
    let second_frame = source.frame(second).unwrap().unwrap();
    assert_eq!(
        second_frame.reference_requirement,
        AlignmentReferenceRequirement::Pending { revision, horizon }
    );
    assert!(second_frame.snapshot.is_some());
    assert!(second_frame.visible_complete());
    assert!(!second_frame.reference_products_current);
    assert_eq!(
        second_frame
            .snapshot
            .as_ref()
            .unwrap()
            .reads()
            .presentation()
            .reference,
        None
    );
    assert!(source.contract(second).unwrap().reference.is_none());
}

#[test]
fn span_and_read_publication_do_not_require_reference_authority() {
    for geometry_lod in [GeometryLod::Span, GeometryLod::Read] {
        let (mut coordinator, camera) = test_coordinator_with_contract(|contract| {
            contract.presentation.geometry_lod = geometry_lod;
        });
        let update = coordinator.update_camera(camera).unwrap();
        assert!(update.foreground_submitted.is_some());
        assert_eq!(
            coordinator.frame().unwrap().reference_requirement,
            AlignmentReferenceRequirement::NotRequired
        );
        pump_until_publication(&mut coordinator);
        let frame = coordinator.frame().unwrap();
        assert!(frame.visible_complete());
        assert!(frame.scene_contracts_current);
        assert_eq!(
            frame.reference_requirement,
            AlignmentReferenceRequirement::NotRequired
        );
        assert_eq!(
            frame
                .snapshot
                .as_ref()
                .unwrap()
                .reads()
                .presentation()
                .geometry_lod,
            geometry_lod
        );
    }
}

#[test]
fn geometric_overlap_does_not_make_a_stale_contract_visible_complete() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);

    let revision = test_reference_revision(4);
    let mut contract = coordinator.contract().clone();
    contract.payload = PayloadContract::new(
        CapabilityRequirement::Required,
        CapabilityRequirement::Required,
        [*b"XS"],
        None,
    );
    contract.presentation.geometry_lod = GeometryLod::Base;
    contract.presentation.reference = Some(revision);
    contract.reference = None;
    let update = coordinator.replace_contract(contract).unwrap();
    assert!(!update.visible_complete);

    let frame = coordinator.frame().unwrap();
    assert!(frame.snapshot.is_some());
    assert!(frame.covered_interval.is_some());
    assert!(!frame.scene_contracts_current);
    assert!(!frame.visible_complete());
    assert!(matches!(
        frame.reference_requirement,
        AlignmentReferenceRequirement::Pending {
            revision: pending,
            ..
        } if pending == revision
    ));
    assert_eq!(
        frame.individual_reads,
        AlignmentIndividualReadCapability::Pending
    );
}
