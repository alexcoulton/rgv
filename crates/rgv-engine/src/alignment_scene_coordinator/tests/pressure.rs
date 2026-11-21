use super::*;

#[test]
fn pressure_aborts_speculation_and_evicts_inactive_pages_without_clearing_current() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    let revision = pump_until_publication(&mut coordinator);

    let mut span = coordinator.contract().presentation;
    span.geometry_lod = GeometryLod::Span;
    assert!(coordinator.prewarm_presentation(span).unwrap());
    let pages_after_prewarm = coordinator.presentation().read_page_count();

    let near_edge_viewport = camera
        .viewport()
        .pan_pixels(340.0, coordinator.contract().contig_length)
        .unwrap();
    let near_edge = AlignmentCamera::from_viewport(
        near_edge_viewport,
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(near_edge).unwrap();
    assert!(update.speculation_submitted.is_some());
    assert!(matches!(
        coordinator.pump_once().unwrap().source_event,
        Some(AlignmentSourceActorEvent::WorkStarted(ref work))
            if work.kind() == crate::SourceIntentKind::Speculative
    ));

    let ledger_before = coordinator.actor().store().ledger().snapshot().unwrap();
    assert!(ledger_before.charged > ledger_before.pinned);
    let pressure = coordinator.reduce_budget(ledger_before.pinned).unwrap();
    assert!(pressure.speculation_was_pending);
    assert!(pressure.active_speculation_aborted);
    assert!(pressure.presentation.evicted_pages() > 0);
    assert!(coordinator.presentation().read_page_count() < pages_after_prewarm);
    assert!(coordinator.actor().schedule().active().is_none());
    assert!(
        coordinator
            .actor()
            .schedule()
            .desired_speculation()
            .is_none()
    );
    assert_eq!(
        coordinator.frame().unwrap().snapshot.unwrap().revision(),
        revision
    );
    assert!(coordinator.frame().unwrap().visible_complete());
    assert_eq!(
        coordinator.telemetry().resource_pressure_speculation_aborts,
        1
    );
    assert_eq!(
        pressure.remaining_overage,
        pressure.charged.saturating_sub(pressure.budget)
    );
}

#[test]
fn foreground_jump_is_latest_wins_and_matches_a_clean_load() {
    let (mut coordinator, initial_camera) = test_coordinator();
    let initial = coordinator.update_camera(initial_camera).unwrap();
    assert!(initial.foreground_submitted.is_some());
    assert!(matches!(
        coordinator.pump_once().unwrap().source_event,
        Some(AlignmentSourceActorEvent::WorkStarted(ref work))
            if work.kind() == crate::SourceIntentKind::Foreground
    ));

    let final_start = coordinator.contract().contig_length.saturating_sub(1_000);
    assert!(final_start > initial_camera.interval().end());
    let final_interval =
        GenomicInterval::new(coordinator.contract().contig, final_start, final_start + 2).unwrap();
    let final_camera = AlignmentCamera::new(
        final_interval,
        0.0,
        initial_camera.row_height(),
        initial_camera.read_height(),
        initial_camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(final_camera).unwrap();
    let latest = update
        .foreground_submitted
        .expect("a disjoint camera jump must replace the old foreground intent");

    let mut published = None;
    for _ in 0..256 {
        let pump = coordinator.pump_once().unwrap();
        if let Some(AlignmentSourceActorEvent::AcquisitionCompleted {
            publish: Some(sequence),
            ..
        }) = pump.source_event
        {
            assert_eq!(sequence, latest, "stale foreground work must never publish");
        }
        if pump.published_scene.is_some() {
            assert!(published.replace(pump.published_scene.unwrap()).is_none());
            break;
        }
    }
    assert!(published.is_some());
    let final_frame = coordinator.frame().unwrap();
    assert!(final_frame.visible_complete());
    assert!(
        coordinator.telemetry().source_yields + coordinator.telemetry().source_retargets > 0,
        "the old in-flight decoder must yield or retarget at a canonical checkpoint"
    );

    let final_scene = coordinator.frame().unwrap().snapshot.unwrap();
    let (mut clean, _) = test_coordinator();
    clean.update_camera(final_camera).unwrap();
    pump_until_publication(&mut clean);
    let clean_scene = clean.frame().unwrap().snapshot.unwrap();
    assert_eq!(
        final_scene.prepared_interval(),
        clean_scene.prepared_interval()
    );
    assert_eq!(
        scene_population(&final_scene),
        scene_population(&clean_scene)
    );
    assert_eq!(
        final_scene.reads().atlas().assignment_fingerprint(),
        clean_scene.reads().atlas().assignment_fingerprint()
    );
    assert_eq!(
        final_scene
            .summaries()
            .project_depth_bins(final_scene.prepared_interval(), 16)
            .unwrap(),
        clean_scene
            .summaries()
            .project_depth_bins(clean_scene.prepared_interval(), 16)
            .unwrap()
    );
}

#[test]
fn pressure_paused_foreground_row_block_does_no_work_until_capacity_camera_or_retry() {
    let (mut coordinator, owner, camera) = published_single_row_block_lane();
    let ledger = coordinator.actor().store().ledger();
    let roomy_budget = ledger.snapshot().unwrap().budget;
    let fill_budget = |ledger: &AllocationLedger| {
        ledger
            .set_budget(ledger.snapshot().unwrap().charged)
            .unwrap();
    };

    fill_budget(&ledger);
    let update = coordinator
        .update_camera(owner, scrolled_camera(camera, 72.0))
        .unwrap();
    assert!(update.published_scene.is_none());
    assert!(coordinator.has_pending_foreground_presentation_work());
    let refused = coordinator.pump_presentation_once().unwrap().unwrap();
    assert_eq!(refused.intent, SourceIntentKind::Foreground);
    assert!(refused.pressure.is_some());
    assert!(!refused.failed);

    // The worker loop asks these questions on every iteration. While the refusal stands
    // there is nothing runnable, nothing is rebuilt, and nothing is reported again.
    let attempts = coordinator
        .telemetry(owner)
        .unwrap()
        .presentation_candidates;
    for _ in 0..64 {
        assert!(!coordinator.has_pending_foreground_presentation_work());
        assert!(!coordinator.has_pending_presentation_work());
        assert!(coordinator.pump_presentation_once().unwrap().is_none());
    }
    assert_eq!(
        coordinator
            .telemetry(owner)
            .unwrap()
            .presentation_candidates,
        attempts
    );
    assert!(
        coordinator
            .owner_work_state(owner)
            .unwrap()
            .presentation_pending,
        "a paused block is still owned work, not an idle worker"
    );
    assert!(coordinator.foreground_presentation_failure(owner).is_none());

    // An explicit retry that is refused again reports through its own result and leaves
    // the lane paused rather than runnable.
    let target = coordinator.frame(owner).unwrap().unwrap().camera.interval();
    let retry = coordinator.retry_resource_pressure(owner, target);
    assert!(retry.is_err_and(|error| coordinator_budget_pressure(&error).is_some()));
    assert!(!coordinator.has_pending_foreground_presentation_work());

    // Ledger progress resumes the block, but not while the driver is still reclaiming.
    ledger.set_budget(roomy_budget).unwrap();
    coordinator.set_capacity_reclaim_active(true);
    assert!(!coordinator.has_pending_foreground_presentation_work());
    coordinator.set_capacity_reclaim_active(false);
    assert!(coordinator.has_pending_foreground_presentation_work());
    let resumed = coordinator.pump_presentation_once().unwrap().unwrap();
    assert!(resumed.published_scene.is_some());
    assert!(
        coordinator
            .frame(owner)
            .unwrap()
            .unwrap()
            .visible_complete()
    );
}

#[test]
fn a_new_camera_or_an_explicit_retry_resumes_a_pressure_paused_row_block() {
    let (mut coordinator, owner, camera) = published_single_row_block_lane();
    let ledger = coordinator.actor().store().ledger();
    let roomy_budget = ledger.snapshot().unwrap().budget;
    ledger
        .set_budget(ledger.snapshot().unwrap().charged)
        .unwrap();
    coordinator
        .update_camera(owner, scrolled_camera(camera, 72.0))
        .unwrap();
    assert!(
        coordinator
            .pump_presentation_once()
            .unwrap()
            .unwrap()
            .pressure
            .is_some()
    );
    assert!(!coordinator.has_pending_foreground_presentation_work());

    // A new camera is a new admission decision: exactly one more attempt, then paused.
    coordinator
        .update_camera(owner, scrolled_camera(camera, 84.0))
        .unwrap();
    assert!(coordinator.has_pending_foreground_presentation_work());
    assert!(
        coordinator
            .pump_presentation_once()
            .unwrap()
            .unwrap()
            .pressure
            .is_some()
    );
    assert!(!coordinator.has_pending_foreground_presentation_work());
    assert!(coordinator.pump_presentation_once().unwrap().is_none());

    // A successful explicit retry clears the pause and publishes.
    ledger.set_budget(roomy_budget).unwrap();
    let target = coordinator.frame(owner).unwrap().unwrap().camera.interval();
    let update = coordinator
        .retry_resource_pressure(owner, target)
        .unwrap()
        .unwrap();
    assert!(update.published_scene.is_some());
    assert!(update.visible_complete);
    assert!(!coordinator.has_pending_presentation_work());
}

#[test]
fn failed_foreground_row_block_is_terminal_for_its_authority_and_owner_scoped() {
    let (mut coordinator, owner, camera) = published_single_row_block_lane();
    let prepared_end = coordinator
        .frame(owner)
        .unwrap()
        .unwrap()
        .snapshot
        .unwrap()
        .prepared_interval()
        .end();
    coordinator
        .update_camera(owner, scrolled_camera(camera, 72.0))
        .unwrap();
    assert!(coordinator.has_pending_foreground_presentation_work());
    // Invalidate only the tile horizon, as the optional-warm failure test does: the visible
    // scene stays valid while the next row-block build fails deterministically.
    let contract = coordinator.contract(owner).unwrap().clone();
    coordinator
        .lanes
        .get_mut(&owner)
        .unwrap()
        .contract
        .contig_length = prepared_end.saturating_sub(1);

    let failed = coordinator
        .pump_presentation_once()
        .expect("a lane-local build failure is not a coordinator error")
        .unwrap();
    assert_eq!(failed.owner, owner);
    assert_eq!(failed.intent, SourceIntentKind::Foreground);
    assert!(failed.failed);
    assert!(failed.pressure.is_none());
    let message = coordinator
        .foreground_presentation_failure(owner)
        .expect("the failure message is retained for its owner");
    assert!(!message.is_empty());

    let attempts = coordinator
        .telemetry(owner)
        .unwrap()
        .presentation_candidates;
    for _ in 0..64 {
        assert!(!coordinator.has_pending_foreground_presentation_work());
        assert!(!coordinator.has_pending_presentation_work());
        assert!(coordinator.pump_presentation_once().unwrap().is_none());
    }
    assert_eq!(
        coordinator
            .telemetry(owner)
            .unwrap()
            .presentation_candidates,
        attempts
    );
    assert!(
        !coordinator
            .owner_work_state(owner)
            .unwrap()
            .presentation_pending,
        "a terminally failed block is not reported as work in progress"
    );
    // Ledger progress and a reclaim ending do not revive a terminal failure.
    coordinator.set_capacity_reclaim_active(false);
    assert!(!coordinator.has_pending_foreground_presentation_work());

    // A contract replacement is a new authority: the pause is gone and the lane converges.
    coordinator
        .lanes
        .get_mut(&owner)
        .unwrap()
        .contract
        .contig_length = contract.contig_length;
    assert!(coordinator.foreground_presentation_failure(owner).is_some());
    let update = coordinator.replace_contract(owner, contract).unwrap();
    assert!(coordinator.foreground_presentation_failure(owner).is_none());
    if update.published_scene.is_none() && !update.visible_complete {
        assert!(coordinator.has_pending_foreground_presentation_work());
        assert!(
            coordinator
                .pump_presentation_once()
                .unwrap()
                .unwrap()
                .published_scene
                .is_some()
        );
    }
    assert!(
        coordinator
            .frame(owner)
            .unwrap()
            .unwrap()
            .visible_complete()
    );
}

#[test]
fn one_owners_scene_build_failure_is_reported_once_and_spares_other_owners() {
    let (mut coordinator, contract, policy, dense_camera) = test_source_coordinator();
    let resident_owner = SourceOwnerId::new(601);
    let failing_owner = SourceOwnerId::new(602);
    let waiting_owner = SourceOwnerId::new(603);
    let small_camera = |start: u64| {
        AlignmentCamera::new(
            GenomicInterval::new(contract.contig, start, start + 2).unwrap(),
            0.0,
            dense_camera.row_height(),
            dense_camera.read_height(),
            dense_camera.clip(),
        )
        .unwrap()
    };
    coordinator
        .insert_lane(resident_owner, contract.clone(), policy)
        .unwrap();
    coordinator
        .update_camera(resident_owner, small_camera(0))
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
        .expect("the resident owner should publish first");

    coordinator
        .insert_lane(failing_owner, contract.clone(), policy)
        .unwrap();
    coordinator
        .insert_lane(waiting_owner, contract.clone(), policy)
        .unwrap();
    coordinator
        .update_camera(failing_owner, dense_camera)
        .unwrap();
    coordinator
        .update_camera(waiting_owner, small_camera(15_000))
        .unwrap();
    assert!(
        coordinator
            .actor()
            .schedule()
            .foreground_pending_for(waiting_owner)
    );
    // Make only the failing owner's candidate impossible to build once its evidence
    // arrives; its camera was validated against the real contig length above.
    coordinator
        .lanes
        .get_mut(&failing_owner)
        .unwrap()
        .contract
        .contig_length = dense_camera.interval().start();

    let mut failures = Vec::new();
    let mut waiting_published = false;
    for _ in 0..2_048 {
        let pump = coordinator
            .pump_once()
            .expect("a lane-local build failure must not fail the shared source pump");
        failures.extend(pump.failures);
        waiting_published |= pump
            .publications
            .iter()
            .any(|publication| publication.owner == waiting_owner);
        if !coordinator.has_pending_source_work() {
            break;
        }
    }
    assert_eq!(
        failures.len(),
        1,
        "the failure is reported once, not on every later source completion"
    );
    assert_eq!(failures[0].owner, failing_owner);
    assert!(!failures[0].message.is_empty());
    assert!(
        waiting_published,
        "another owner's queued foreground must still run to publication"
    );
    let resident = coordinator.frame(resident_owner).unwrap().unwrap();
    assert!(resident.visible_complete());
    assert_eq!(
        resident.snapshot.as_ref().map(|scene| scene.revision()),
        Some(resident_revision)
    );
    let schedule = coordinator.actor().schedule();
    assert!(
        schedule.desired_foreground_for(failing_owner).is_none(),
        "the failed owner must not restart the same source target"
    );
}

#[test]
fn a_stronger_payload_contract_is_not_satisfied_by_a_scene_built_without_it() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    assert!(coordinator.frame().unwrap().visible_complete());

    // Same presentation, summaries, layout, and filters; only the retained record payload
    // grows. Every reuse predicate must agree that the resident evidence no longer suffices.
    let mut contract = coordinator.contract().clone();
    contract.payload = PayloadContract::new(
        CapabilityRequirement::Required,
        CapabilityRequirement::Required,
        [*b"XS"],
        None,
    );
    let required = contract.payload.clone();
    let update = coordinator.replace_contract(contract).unwrap();
    assert!(update.published_scene.is_none());
    assert!(!update.visible_complete);
    assert!(
        update.foreground_submitted.is_some(),
        "the missing payload must be acquired, not assumed"
    );
    let frame = coordinator.frame().unwrap();
    assert!(!frame.scene_contracts_current);
    assert!(!frame.visible_complete());
    assert!(!coordinator.has_pending_presentation_work());

    pump_until_publication(&mut coordinator);
    let frame = coordinator.frame().unwrap();
    assert!(frame.visible_complete());
    assert!(
        frame
            .snapshot
            .unwrap()
            .reads()
            .evidence()
            .payload()
            .satisfies(&required)
    );
}

#[test]
fn post_acquisition_root_pressure_keeps_another_owner_foreground_queued() {
    let (mut coordinator, contract, policy, dense_camera) = test_source_coordinator_with_ledger(
        AllocationLedger::new_with_transient_budget(64 << 20, 0, 64 << 20),
    );
    let pressured_owner = SourceOwnerId::new(501);
    let waiting_owner = SourceOwnerId::new(502);
    let waiting_camera = AlignmentCamera::new(
        GenomicInterval::new(contract.contig, 0, 2).unwrap(),
        0.0,
        dense_camera.row_height(),
        dense_camera.read_height(),
        dense_camera.clip(),
    )
    .unwrap();
    coordinator
        .insert_lane(pressured_owner, contract.clone(), policy)
        .unwrap();
    coordinator
        .insert_lane(waiting_owner, contract, policy)
        .unwrap();
    coordinator
        .update_camera(pressured_owner, dense_camera)
        .unwrap();
    coordinator
        .update_camera(waiting_owner, waiting_camera)
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
        "the first foreground should admit a canonical chunk"
    );
    let ledger = coordinator.actor().store().ledger();
    let charged = ledger.snapshot().unwrap().charged;
    ledger.set_budget(charged).unwrap();

    let pressures = (0..256)
        .find_map(|_| {
            let pressures = coordinator.pump_once().unwrap().pressures;
            (!pressures.is_empty()).then_some(pressures)
        })
        .expect("the first owner's scene-root admission should encounter pressure");
    assert_eq!(pressures.len(), 1);
    assert_eq!(pressures[0].owner, pressured_owner);
    assert_eq!(pressures[0].intent, SourceIntentKind::Foreground);
    assert!(
        coordinator
            .actor()
            .schedule()
            .desired_foreground_for(pressured_owner)
            .is_none()
    );
    assert!(
        coordinator
            .actor()
            .schedule()
            .desired_foreground_for(waiting_owner)
            .is_some(),
        "owner-local pressure must not cancel another lane's queued foreground"
    );
    assert!(coordinator.has_pending_source_work());
}

#[test]
fn allocation_pressure_is_classified_through_every_transparent_wrapper() {
    use rgv_alignment_scene::{
        AlignmentPresentationStoreError, AlignmentRowAtlasError, EvidenceBuildError,
        EvidenceSelectionError, EvidenceStoreError, SemanticPageError,
    };

    use crate::alignment_scene_coordinator::pressure::AllocationPressure;

    type Error = AlignmentSceneCoordinatorError<()>;

    let pressure = LedgerError::BudgetExceeded {
        requested: 8,
        charged: 4,
        reserved: 0,
        limit: 8,
    };
    let atlas = EvidenceStoreError::RowAtlas(AlignmentRowAtlasError::EvidenceSelection(
        EvidenceSelectionError::Ledger(pressure),
    ));
    // `#[error(transparent)]` hides the wrapped ledger error from a `source()` walk, which is
    // why classification is typed.
    assert!(std::error::Error::source(&atlas).is_none());
    assert_eq!(atlas.allocation_pressure(), Some(pressure));
    // Wrappers the hand-written matchers used to miss.
    let build = EvidenceStoreError::Build(EvidenceBuildError::Selection(
        EvidenceSelectionError::Ledger(pressure),
    ));
    assert_eq!(build.allocation_pressure(), Some(pressure));
    let page = AlignmentPresentationStoreError::SemanticPage(SemanticPageError::EvidenceSelection(
        EvidenceSelectionError::Ledger(pressure),
    ));
    assert_eq!(page.allocation_pressure(), Some(pressure));

    assert_eq!(
        coordinator_budget_pressure(&Error::Store(atlas)),
        Some(pressure)
    );
    assert_eq!(
        coordinator_budget_pressure(&Error::Presentation(page)),
        Some(pressure)
    );
    // Bookkeeping corruption is never retryable pressure, however it is wrapped.
    for corrupt in [LedgerError::Poisoned, LedgerError::IdOverflow] {
        assert_eq!(
            coordinator_budget_pressure(&Error::Store(EvidenceStoreError::Ledger(corrupt))),
            None
        );
    }
    assert_eq!(coordinator_budget_pressure(&Error::MissingCamera), None);
}
