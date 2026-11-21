use super::*;

#[test]
fn preparation_policy_changes_apply_immediately_without_a_hidden_clamp() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let initial = coordinator
        .frame()
        .unwrap()
        .snapshot
        .unwrap()
        .prepared_interval();

    let expanded = AlignmentScenePreparationPolicy::new(2.0, 3.0, 0.5, 12, 2).unwrap();
    let update = coordinator
        .set_preparation_policy(expanded)
        .unwrap()
        .unwrap();
    assert_eq!(coordinator.preparation_policy(), expanded);
    assert!(update.visible_complete);
    assert!(update.speculation_submitted.is_some());
    let desired = coordinator.desired_prepared_interval().unwrap();
    assert!(desired.start() < initial.start());
    assert!(desired.end() > initial.end());

    let mut invalid = expanded;
    invalid.ahead_viewports = f64::INFINITY;
    assert!(matches!(
        coordinator.set_preparation_policy(invalid),
        Err(AlignmentSceneCoordinatorError::Config(
            AlignmentSceneCoordinatorConfigError::InvalidPreparationDistance
        ))
    ));
    assert_eq!(coordinator.preparation_policy(), expanded);
}

#[test]
fn camera_outside_the_contig_is_refused_instead_of_silently_clamped() {
    let (mut coordinator, camera) = test_coordinator();
    let invalid = AlignmentCamera::new(
        GenomicInterval::new(
            coordinator.contract().contig,
            coordinator.contract().contig_length,
            coordinator.contract().contig_length + 1,
        )
        .unwrap(),
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    assert!(matches!(
        coordinator.update_camera(invalid),
        Err(AlignmentSceneCoordinatorError::CameraOutsideContig { .. })
    ));
    assert!(coordinator.frame().is_none());
}

#[test]
fn exact_dense_refusal_is_terminal_retains_current_and_retries_after_limit_change() {
    let (mut coordinator, contract, policy, dense_camera) = test_source_coordinator();
    let owner = SourceOwnerId::new(303);
    coordinator.insert_lane(owner, contract, policy).unwrap();

    let empty_camera = AlignmentCamera::new(
        GenomicInterval::new(dense_camera.interval().contig(), 0, 2).unwrap(),
        0.0,
        dense_camera.row_height(),
        dense_camera.read_height(),
        dense_camera.clip(),
    )
    .unwrap();
    coordinator.update_camera(owner, empty_camera).unwrap();
    let initial_revision = (0..256)
        .find_map(|_| {
            coordinator
                .pump_once()
                .unwrap()
                .publications
                .into_iter()
                .find(|publication| publication.owner == owner)
                .map(|publication| publication.revision)
        })
        .expect("the empty foreground target should publish");
    assert!(
        coordinator
            .frame(owner)
            .unwrap()
            .unwrap()
            .visible_complete()
    );

    coordinator
        .actor_mut()
        .submit_control(LocalBamSourceControl::SetCompleteReadLimit(
            NonZeroUsize::new(1).unwrap(),
        ))
        .unwrap();
    assert!(matches!(
        coordinator.pump_once().unwrap().source_event,
        Some(AlignmentSourceActorEvent::ControlApplied)
    ));

    coordinator.update_camera(owner, dense_camera).unwrap();
    let refusal = (0..256)
        .find_map(|_| {
            coordinator
                .pump_once()
                .unwrap()
                .refusals
                .into_iter()
                .find(|refusal| refusal.owner == owner)
        })
        .expect("the dense foreground target should refuse individual reads");
    assert_eq!(refusal.complete_read_limit, 1);
    assert_eq!(refusal.eligible_records, 13);

    let refused = coordinator.frame(owner).unwrap().unwrap();
    assert_eq!(
        refused.snapshot.as_ref().map(|scene| scene.revision()),
        Some(initial_revision),
        "a refused candidate must not erase or replace the immutable current scene"
    );
    assert!(!refused.visible_complete());
    assert_eq!(
        refused.individual_reads,
        AlignmentIndividualReadCapability::Refused {
            target: refusal.target,
            complete_read_limit: 1,
            eligible_records: 13,
        }
    );
    assert!(
        !coordinator.has_pending_source_work(),
        "a terminal refusal must not resubmit the same target in a loop"
    );
    let idle = coordinator.pump_once().unwrap();
    assert!(idle.source_event.is_none());
    assert!(idle.refusals.is_empty());

    coordinator
        .actor_mut()
        .submit_control(LocalBamSourceControl::SetCompleteReadLimit(
            NonZeroUsize::new(1_000).unwrap(),
        ))
        .unwrap();
    let retries = coordinator.retry_refused_lanes().unwrap();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].owner, owner);
    assert_eq!(
        coordinator.frame(owner).unwrap().unwrap().individual_reads,
        AlignmentIndividualReadCapability::Pending
    );
    assert!(matches!(
        coordinator.pump_once().unwrap().source_event,
        Some(AlignmentSourceActorEvent::ControlApplied)
    ));

    let final_revision = (0..256)
        .find_map(|_| {
            coordinator
                .pump_once()
                .unwrap()
                .publications
                .into_iter()
                .find(|publication| publication.owner == owner)
                .map(|publication| publication.revision)
        })
        .expect("raising the limit should publish the previously refused target");
    let complete = coordinator.frame(owner).unwrap().unwrap();
    assert!(complete.visible_complete());
    assert_eq!(
        complete.individual_reads,
        AlignmentIndividualReadCapability::Complete {
            scene_revision: final_revision,
        }
    );
}

#[test]
fn narrower_camera_restarts_after_a_containing_foreground_refusal() {
    let (mut coordinator, contract, policy, camera) = test_source_coordinator();
    let owner = SourceOwnerId::new(304);
    coordinator.insert_lane(owner, contract, policy).unwrap();
    coordinator
        .actor_mut()
        .submit_control(LocalBamSourceControl::SetCompleteReadLimit(
            NonZeroUsize::new(1).unwrap(),
        ))
        .unwrap();
    assert!(matches!(
        coordinator.pump_once().unwrap().source_event,
        Some(AlignmentSourceActorEvent::ControlApplied)
    ));

    let broad = AlignmentCamera::new(
        GenomicInterval::new(camera.interval().contig(), 0, 10_100).unwrap(),
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    coordinator.update_camera(owner, broad).unwrap();
    let refused_work = (0..256)
        .find_map(|_| {
            let pump = coordinator.pump_once().unwrap();
            pump.refusals
                .iter()
                .any(|refusal| refusal.owner == owner)
                .then(|| pump.source_event)
                .flatten()
        })
        .expect("the broad exact camera should exceed the limit");
    let AlignmentSourceActorEvent::AcquisitionRefused { work, .. } = refused_work else {
        panic!("a typed refusal must carry the completed source work")
    };
    assert!(
        work.intent()
            .spec()
            .target()
            .contains_interval(GenomicInterval::new(camera.interval().contig(), 0, 1).unwrap())
    );
    assert!(!coordinator.has_pending_source_work());

    let narrow = AlignmentCamera::new(
        GenomicInterval::new(camera.interval().contig(), 0, 1).unwrap(),
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(owner, narrow).unwrap();
    assert!(
        update.foreground_submitted.is_some(),
        "settled refused intent must not masquerade as pending work for a new camera"
    );
    assert!(coordinator.owner_work_state(owner).unwrap().source_pending);
    let revision = (0..256)
        .find_map(|_| {
            coordinator
                .pump_once()
                .unwrap()
                .publications
                .into_iter()
                .find(|publication| publication.owner == owner)
                .map(|publication| publication.revision)
        })
        .expect("the narrower admissible camera should publish");
    let frame = coordinator.frame(owner).unwrap().unwrap();
    assert!(frame.visible_complete());
    assert_eq!(
        frame.individual_reads,
        AlignmentIndividualReadCapability::Complete {
            scene_revision: revision,
        }
    );
}

#[test]
fn offscreen_buffer_population_does_not_affect_visible_admission() {
    let (mut coordinator, mut contract, _, camera) = test_source_coordinator();
    let owner = SourceOwnerId::new(305);
    let policy = AlignmentScenePreparationPolicy::new(10_000.0, 10_000.0, 0.25, 4, 1).unwrap();
    contract.complete_read_limit = NonZeroUsize::new(1).unwrap();
    coordinator.insert_lane(owner, contract, policy).unwrap();
    coordinator
        .actor_mut()
        .submit_control(LocalBamSourceControl::SetCompleteReadLimit(
            NonZeroUsize::new(1).unwrap(),
        ))
        .unwrap();
    assert!(matches!(
        coordinator.pump_once().unwrap().source_event,
        Some(AlignmentSourceActorEvent::ControlApplied)
    ));

    let visible = AlignmentCamera::new(
        GenomicInterval::new(camera.interval().contig(), 0, 1).unwrap(),
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(owner, visible).unwrap();
    assert!(update.foreground_submitted.is_some());
    let first_work = loop {
        let pump = coordinator.pump_once().unwrap();
        if let Some(AlignmentSourceActorEvent::WorkStarted(work)) = pump.source_event {
            break work;
        }
    };
    assert_eq!(first_work.kind(), SourceIntentKind::Foreground);
    assert_eq!(
        first_work.intent().spec().admission_target(),
        visible.interval()
    );
    assert!(
        first_work.intent().spec().target().len() > visible.interval().len(),
        "the physical acquisition should retain a coherent offscreen browsing horizon"
    );

    let revision = (0..256)
        .find_map(|_| {
            coordinator
                .pump_once()
                .unwrap()
                .publications
                .into_iter()
                .find(|publication| publication.owner == owner)
                .map(|publication| publication.revision)
        })
        .expect("the empty visible camera should admit its coherent buffered scene");
    let frame = coordinator.frame(owner).unwrap().unwrap();
    assert!(frame.visible_complete());
    assert_eq!(
        frame.individual_reads,
        AlignmentIndividualReadCapability::Complete {
            scene_revision: revision,
        }
    );
    assert!(
        frame
            .snapshot
            .unwrap()
            .prepared_interval()
            .contains_interval(visible.interval())
    );

    let update = coordinator.update_camera(owner, camera).unwrap();
    assert!(update.foreground_submitted.is_none());
    let dense = coordinator.frame(owner).unwrap().unwrap();
    assert!(dense.visible_complete());
    assert!(matches!(
        dense.individual_reads,
        AlignmentIndividualReadCapability::Refused {
            target,
            complete_read_limit: 1,
            eligible_records,
        } if target == camera.interval() && eligible_records > 1
    ));
}

#[test]
fn equal_panel_lanes_share_one_query_authority_at_identical_page_identity() {
    let (mut coordinator, contract, policy, camera) = test_source_coordinator();
    let first = SourceOwnerId::new(101);
    let second = SourceOwnerId::new(202);
    coordinator
        .insert_lane(first, contract.clone(), policy)
        .unwrap();
    coordinator.insert_lane(second, contract, policy).unwrap();
    assert!(
        coordinator
            .update_camera(first, camera)
            .unwrap()
            .foreground_submitted
            .is_some()
    );
    assert!(
        coordinator
            .update_camera(second, camera)
            .unwrap()
            .foreground_submitted
            .is_some()
    );

    let mut publications = BTreeMap::new();
    for _ in 0..256 {
        let pump = coordinator.pump_once().unwrap();
        for publication in pump.publications {
            assert!(
                publications
                    .insert(publication.owner, publication.revision)
                    .is_none(),
                "an owner must publish at most once for an unchanged camera"
            );
        }
        if publications.len() == 2 {
            break;
        }
    }
    assert_eq!(publications.len(), 2);
    assert_eq!(coordinator.actor().store().acquisition_count(), 1);
    assert_eq!(coordinator.presentation().owner_count(), 2);
    assert!(
        coordinator
            .frame(first)
            .unwrap()
            .unwrap()
            .visible_complete()
    );
    assert!(
        coordinator
            .frame(second)
            .unwrap()
            .unwrap()
            .visible_complete()
    );
    assert_eq!(
        coordinator.telemetry(first).unwrap().source_work_started
            + coordinator.telemetry(second).unwrap().source_work_started,
        1
    );

    let first_scene = coordinator.frame(first).unwrap().unwrap().snapshot.unwrap();
    let second_scene = coordinator
        .frame(second)
        .unwrap()
        .unwrap()
        .snapshot
        .unwrap();
    assert!(Arc::ptr_eq(
        first_scene.reads().evidence(),
        second_scene.reads().evidence()
    ));
    assert!(Arc::ptr_eq(
        first_scene.reads().atlas(),
        second_scene.reads().atlas()
    ));
    assert_eq!(
        first_scene.reads().pages().len(),
        second_scene.reads().pages().len()
    );
    assert!(
        first_scene
            .reads()
            .pages()
            .iter()
            .zip(second_scene.reads().pages())
            .all(|(left, right)| Arc::ptr_eq(left, right))
    );
    assert!(
        coordinator
            .actor()
            .schedule()
            .desired_foreground_for(first)
            .is_none()
    );
    assert!(
        coordinator
            .actor()
            .schedule()
            .desired_foreground_for(second)
            .is_none()
    );

    assert!(coordinator.remove_lane(first));
    assert_eq!(coordinator.lane_count(), 1);
    assert_eq!(coordinator.presentation().owner_count(), 1);
    assert!(coordinator.frame(second).unwrap().is_some());
}

#[test]
fn disjoint_panel_foregrounds_complete_serially_without_restart_livelock() {
    let (mut coordinator, contract, policy, first_camera) = test_source_coordinator();
    let first = SourceOwnerId::new(11);
    let second = SourceOwnerId::new(22);
    let second_start = contract.contig_length.saturating_sub(1_000);
    let second_camera = AlignmentCamera::new(
        GenomicInterval::new(contract.contig, second_start, second_start + 2).unwrap(),
        0.0,
        first_camera.row_height(),
        first_camera.read_height(),
        first_camera.clip(),
    )
    .unwrap();
    coordinator
        .insert_lane(first, contract.clone(), policy)
        .unwrap();
    coordinator.insert_lane(second, contract, policy).unwrap();
    coordinator.update_camera(first, first_camera).unwrap();
    coordinator.update_camera(second, second_camera).unwrap();

    let mut published = BTreeMap::new();
    for _ in 0..512 {
        let pump = coordinator.pump_once().unwrap();
        for publication in pump.publications {
            published.insert(publication.owner, publication.revision);
        }
        if published.len() == 2 {
            break;
        }
    }
    assert_eq!(published.len(), 2);
    assert_eq!(coordinator.actor().store().acquisition_count(), 2);
    assert_eq!(
        coordinator.telemetry(first).unwrap().source_work_started
            + coordinator.telemetry(second).unwrap().source_work_started,
        2
    );
    assert!(
        coordinator
            .frame(first)
            .unwrap()
            .unwrap()
            .visible_complete()
    );
    assert!(
        coordinator
            .frame(second)
            .unwrap()
            .unwrap()
            .visible_complete()
    );
}

#[test]
fn post_acquisition_root_pressure_is_owner_scoped_and_preserves_other_lane() {
    let (mut coordinator, contract, policy, dense_camera) = test_source_coordinator_with_ledger(
        AllocationLedger::new_with_transient_budget(64 << 20, 0, 64 << 20),
    );
    let resident_owner = SourceOwnerId::new(401);
    let pressured_owner = SourceOwnerId::new(402);
    coordinator
        .insert_lane(resident_owner, contract.clone(), policy)
        .unwrap();
    let resident_camera = AlignmentCamera::new(
        GenomicInterval::new(contract.contig, 0, 2).unwrap(),
        0.0,
        dense_camera.row_height(),
        dense_camera.read_height(),
        dense_camera.clip(),
    )
    .unwrap();
    coordinator
        .update_camera(resident_owner, resident_camera)
        .unwrap();
    let resident_revision = (0..256)
        .find_map(|_| {
            coordinator
                .pump_once()
                .unwrap()
                .publications
                .into_iter()
                .find(|publication| publication.owner == resident_owner)
                .map(|publication| publication.revision)
        })
        .expect("the resident owner should publish before pressure is introduced");

    coordinator
        .insert_lane(pressured_owner, contract, policy)
        .unwrap();
    coordinator
        .update_camera(pressured_owner, dense_camera)
        .unwrap();
    let mut observed_chunk = false;
    for _ in 0..256 {
        let pump = coordinator.pump_once().unwrap();
        if matches!(
            pump.source_event,
            Some(AlignmentSourceActorEvent::ChunkAdmitted { .. })
        ) {
            observed_chunk = true;
            break;
        }
    }
    assert!(
        observed_chunk,
        "the dense target should admit a canonical chunk"
    );
    let ledger = coordinator.actor().store().ledger();
    let charged = ledger.snapshot().unwrap().charged;
    ledger.set_budget(charged).unwrap();

    let pressures = (0..256)
        .find_map(|_| {
            let pressures = coordinator.pump_once().unwrap().pressures;
            (!pressures.is_empty()).then_some(pressures)
        })
        .expect("scene-root admission should report owner-local pressure");
    assert_eq!(
        pressures.len(),
        1,
        "a failed candidate must not widen pressure to another lane"
    );
    let pressure = pressures[0];
    assert_eq!(pressure.owner, pressured_owner);
    assert_eq!(pressure.intent, SourceIntentKind::Foreground);
    assert!(matches!(
        pressure.pressure,
        LedgerError::BudgetExceeded { .. }
            | LedgerError::TransientBudgetExceeded { .. }
            | LedgerError::ReplacementTransitionActive { .. }
    ));
    assert_eq!(pressure.target.contig(), dense_camera.interval().contig());
    assert!(pressure.target.contains_interval(dense_camera.interval()));

    let resident = coordinator.frame(resident_owner).unwrap().unwrap();
    assert!(resident.visible_complete());
    assert_eq!(
        resident.snapshot.as_ref().map(|scene| scene.revision()),
        Some(resident_revision),
        "another owner's failed candidate must not replace the resident immutable scene"
    );
    assert!(
        coordinator
            .actor()
            .schedule()
            .desired_foreground_for(resident_owner)
            .is_none()
    );
    assert!(
        coordinator
            .actor()
            .schedule()
            .desired_foreground_for(pressured_owner)
            .is_none(),
        "the failed owner must not restart the same source target in a busy loop"
    );
    assert!(
        !coordinator
            .frame(pressured_owner)
            .unwrap()
            .unwrap()
            .visible_complete()
    );
}
