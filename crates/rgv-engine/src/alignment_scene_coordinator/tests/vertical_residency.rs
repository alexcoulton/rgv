use super::*;

#[test]
fn missing_vertical_block_is_deferred_without_reopening_source_authority() {
    let (mut coordinator, camera) = test_coordinator_with_contract(|contract| {
        contract.row_block_rows = NonZeroU32::new(1).unwrap();
    });
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let before = coordinator.telemetry();
    let atlas_revision = coordinator
        .frame()
        .unwrap()
        .snapshot
        .as_ref()
        .unwrap()
        .reads()
        .atlas()
        .revision();
    let scrolled = AlignmentCamera::from_viewport(
        camera.viewport(),
        72.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();

    let update = coordinator.update_camera(scrolled).unwrap();
    assert!(update.published_scene.is_none());
    assert!(update.foreground_submitted.is_none());
    assert!(!update.visible_complete);
    assert!(coordinator.has_pending_presentation_work());
    assert_eq!(
        coordinator.telemetry().row_camera_updates_deferred,
        before.row_camera_updates_deferred + 1
    );

    let pump = coordinator.pump_presentation_once().unwrap().unwrap();
    assert_eq!(pump.intent, SourceIntentKind::Foreground);
    assert!(pump.published_scene.is_some());
    assert!(coordinator.frame().unwrap().visible_complete());
    let after = coordinator.telemetry();
    assert_eq!(after.source_work_started, before.source_work_started);
    assert_eq!(after.evidence_candidates, before.evidence_candidates);
    assert_eq!(after.row_atlas_candidates, before.row_atlas_candidates);
    assert_eq!(after.foreground_row_block_publications, 1);
    assert_eq!(
        coordinator
            .frame()
            .unwrap()
            .snapshot
            .unwrap()
            .reads()
            .atlas()
            .revision(),
        atlas_revision
    );
}

#[test]
fn visible_scene_publishes_before_incremental_full_vertical_residency() {
    let (mut coordinator, camera) = test_coordinator();
    let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
        .unwrap()
        .with_full_vertical_residency(true);
    assert!(
        coordinator
            .set_preparation_policy(policy)
            .unwrap()
            .is_none()
    );
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);

    let initial = coordinator.frame().unwrap();
    assert!(initial.visible_complete());
    let initial_scene = initial.snapshot.unwrap();
    let total_rows = u32::try_from(initial_scene.reads().atlas().row_count()).unwrap();
    let initial_rows = initial_scene.reads().prepared_rows();
    assert!(initial_rows.end.saturating_sub(initial_rows.start) < total_rows);
    assert!(matches!(
        initial.vertical_residency,
        AlignmentVerticalResidency::Preparing { .. }
    ));
    let source_work = coordinator.telemetry().source_work_started;
    let atlas_revision = initial_scene.reads().atlas().revision();

    coordinator.set_optional_work_paused(true).unwrap();
    assert!(!coordinator.has_pending_presentation_work());
    assert!(coordinator.pump_presentation_once().unwrap().is_none());
    assert_eq!(coordinator.telemetry().optional_work_pauses, 1);
    assert_eq!(
        coordinator.frame().unwrap().snapshot.unwrap().revision(),
        initial_scene.revision()
    );
    coordinator.set_optional_work_paused(false).unwrap();
    assert!(coordinator.has_pending_presentation_work());
    assert_eq!(coordinator.telemetry().optional_work_resumes, 1);

    let mut checkpoints = 0_usize;
    let mut cumulative_read_page_occurrences = 0_u64;
    let mut previous_warmed_rows = match initial.vertical_residency {
        AlignmentVerticalResidency::Preparing { prepared_rows, .. } => prepared_rows,
        state => panic!("initial bounded scene should still be warming, got {state:?}"),
    };
    while coordinator.has_pending_presentation_work() {
        let pump = coordinator
            .pump_presentation_once()
            .unwrap()
            .expect("pending vertical work produces one checkpoint");
        assert!(!pump.budget_limited);
        assert!(pump.published_scene.is_none());
        checkpoints = checkpoints.saturating_add(1);
        assert!(checkpoints < 256, "vertical warmup did not converge");

        let telemetry = coordinator.telemetry();
        let trace = telemetry
            .last_vertical_residency_pump
            .expect("a vertical checkpoint records its exact trace state");
        let retained_frame = coordinator.frame().unwrap();
        let retained = retained_frame.snapshot.unwrap();
        assert_eq!(trace.checkpoint, telemetry.vertical_residency_checkpoints);
        assert_eq!(trace.current_scene, initial_scene.revision());
        assert_eq!(trace.published_scene, None);
        assert_eq!(
            trace.current_prepared_rows,
            initial_scene.reads().prepared_rows().into()
        );
        assert_eq!(
            trace.current_read_pages,
            usize_as_u64(initial_scene.reads().pages().len())
        );
        assert_eq!(
            trace.current_summary_pages,
            usize_as_u64(initial_scene.summary_pages().len())
        );
        assert!(trace.target_read_pages.is_some_and(|pages| pages > 0));
        assert!(trace.target_read_page_cache_misses.is_some());
        assert_eq!(trace.target_summary_pages, None);
        assert!(trace.pump_elapsed_nanoseconds >= trace.prepare_elapsed_nanoseconds);
        cumulative_read_page_occurrences =
            cumulative_read_page_occurrences.saturating_add(trace.target_read_pages.unwrap());
        assert_eq!(
            telemetry.vertical_residency_read_page_occurrences,
            cumulative_read_page_occurrences
        );
        assert_eq!(telemetry.vertical_residency_summary_page_occurrences, 0);
        assert!(
            telemetry.vertical_residency_prepare_nanoseconds >= trace.prepare_elapsed_nanoseconds
        );
        assert!(telemetry.vertical_residency_pump_nanoseconds >= trace.pump_elapsed_nanoseconds);
        assert_eq!(retained.revision(), initial_scene.revision());
        assert_eq!(retained.reads().prepared_rows(), initial_rows);
        let warmed_rows = match retained_frame.vertical_residency {
            AlignmentVerticalResidency::Preparing { prepared_rows, .. }
            | AlignmentVerticalResidency::BudgetLimited { prepared_rows, .. }
            | AlignmentVerticalResidency::Unavailable { prepared_rows, .. } => prepared_rows,
            AlignmentVerticalResidency::Complete { total_rows } => total_rows,
            AlignmentVerticalResidency::ViewportBounded => {
                panic!("full vertical residency policy unexpectedly became viewport-bounded")
            }
        };
        assert!(warmed_rows > previous_warmed_rows);
        previous_warmed_rows = warmed_rows;
        assert_eq!(pump.completed, warmed_rows == total_rows);
    }

    let complete = coordinator.frame().unwrap();
    assert!(matches!(
        complete.vertical_residency,
        AlignmentVerticalResidency::Complete {
            total_rows: completed
        } if completed == total_rows
    ));
    let complete_scene = complete.snapshot.unwrap();
    assert_eq!(complete_scene.revision(), initial_scene.revision());
    assert_eq!(complete_scene.reads().prepared_rows(), initial_rows);
    assert_eq!(complete_scene.reads().atlas().revision(), atlas_revision);
    let warmed = coordinator.telemetry();
    assert_eq!(warmed.source_work_started, source_work);
    assert_eq!(warmed.vertical_residency_checkpoints as usize, checkpoints);
    assert_eq!(warmed.vertical_residency_publications, 0);
    assert_eq!(
        warmed.vertical_residency_blocks_warmed as usize,
        checkpoints
    );
    assert_eq!(warmed.vertical_residency_completions, 1);
    assert_eq!(warmed.vertical_residency_budget_limits, 0);
    assert_eq!(warmed.vertical_residency_failures, 0);
    assert_eq!(
        warmed
            .last_vertical_residency_pump
            .expect("the completed pass retains its last checkpoint")
            .outcome,
        AlignmentVerticalResidencyPumpOutcome::Completed
    );

    let bottom_scroll = f64::from(total_rows.saturating_sub(1)) * camera.row_height();
    let bottom = AlignmentCamera::from_viewport(
        camera.viewport(),
        bottom_scroll,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let bottom_prepared = prepared_interval(
        bottom.into(),
        CameraDirection::Stationary,
        coordinator.preparation_policy(),
        coordinator.contract(),
    )
    .unwrap();
    assert_eq!(bottom_prepared, initial_scene.prepared_interval());
    let warmed_page_count = coordinator.presentation().read_page_count();
    let update = coordinator.update_camera(bottom).unwrap();
    assert!(update.visible_complete);
    assert!(!update.reused_current_scene);
    assert!(update.published_scene.is_some());
    assert_eq!(
        coordinator.telemetry().presentation_candidates,
        warmed.presentation_candidates + 1
    );
    assert_eq!(
        coordinator.presentation().read_page_count(),
        warmed_page_count
    );
}

#[test]
fn unexpected_vertical_warm_failure_is_latched_without_failing_the_visible_scene() {
    let (mut coordinator, camera) = test_coordinator();
    let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
        .unwrap()
        .with_full_vertical_residency(true);
    assert!(
        coordinator
            .set_preparation_policy(policy)
            .unwrap()
            .is_none()
    );
    coordinator.update_camera(camera).unwrap();
    let visible_revision = pump_until_publication(&mut coordinator);
    let prepared_end = coordinator
        .frame()
        .unwrap()
        .snapshot
        .as_ref()
        .unwrap()
        .prepared_interval()
        .end();
    assert!(coordinator.has_pending_presentation_work());

    // Deliberately invalidate only the optional tile horizon after the visible scene has
    // committed. This exercises the defensive failure path without manufacturing malformed
    // evidence or atlas roots, which their public constructors correctly forbid.
    let owner = coordinator.owner;
    coordinator
        .source
        .lanes
        .get_mut(&owner)
        .unwrap()
        .contract
        .contig_length = prepared_end.saturating_sub(1);

    let failed = coordinator
        .pump_presentation_once()
        .unwrap()
        .expect("one optional checkpoint observes the injected failure");
    assert!(failed.failed);
    assert!(!failed.budget_limited);
    assert!(!failed.completed);
    assert!(!coordinator.has_pending_presentation_work());
    assert!(coordinator.pump_presentation_once().unwrap().is_none());
    assert_eq!(coordinator.telemetry().vertical_residency_failures, 1);

    let retained = coordinator.frame().unwrap();
    assert!(retained.visible_complete());
    assert_eq!(retained.snapshot.unwrap().revision(), visible_revision);
    assert!(matches!(
        retained.vertical_residency,
        AlignmentVerticalResidency::Unavailable { .. }
    ));
}

#[test]
fn vertical_residency_warm_blocks_are_nearest_first_and_never_repeat() {
    let block = NonZeroU32::new(8).unwrap();
    let mut warmed = BTreeSet::new();
    assert_eq!(
        next_unwarmed_vertical_block(&warmed, 30, block, 10..14),
        Some(CanonicalRowBlock::new(1, block))
    );
    warmed.insert(CanonicalRowBlock::new(1, block));
    assert_eq!(
        next_unwarmed_vertical_block(&warmed, 30, block, 10..14),
        Some(CanonicalRowBlock::new(2, block))
    );
    warmed.insert(CanonicalRowBlock::new(2, block));
    assert_eq!(
        next_unwarmed_vertical_block(&warmed, 30, block, 10..14),
        Some(CanonicalRowBlock::new(0, block))
    );
    warmed.insert(CanonicalRowBlock::new(0, block));
    assert_eq!(
        next_unwarmed_vertical_block(&warmed, 30, block, 10..14),
        Some(CanonicalRowBlock::new(3, block))
    );
    warmed.insert(CanonicalRowBlock::new(3, block));
    assert_eq!(
        next_unwarmed_vertical_block(&warmed, 30, block, 10..14),
        None
    );
}

#[test]
fn vertical_residency_budget_limit_preserves_visible_authority_and_retries_after_increase() {
    let (mut coordinator, camera) = test_coordinator();
    let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
        .unwrap()
        .with_full_vertical_residency(true);
    assert!(
        coordinator
            .set_preparation_policy(policy)
            .unwrap()
            .is_none()
    );
    coordinator.update_camera(camera).unwrap();
    let visible_revision = pump_until_publication(&mut coordinator);
    let charged = coordinator
        .actor()
        .store()
        .ledger()
        .snapshot()
        .unwrap()
        .charged;
    coordinator.reduce_budget(charged).unwrap();

    let limited = coordinator
        .pump_presentation_once()
        .unwrap()
        .expect("full vertical residency is initially pending");
    assert!(limited.budget_limited);
    assert!(limited.published_scene.is_none());
    assert!(!coordinator.has_pending_presentation_work());
    let frame = coordinator.frame().unwrap();
    assert!(frame.visible_complete());
    assert_eq!(frame.snapshot.unwrap().revision(), visible_revision);
    assert!(matches!(
        frame.vertical_residency,
        AlignmentVerticalResidency::BudgetLimited { .. }
    ));
    assert_eq!(coordinator.telemetry().vertical_residency_budget_limits, 1);
    let telemetry = coordinator.telemetry();
    let trace = telemetry
        .last_vertical_residency_pump
        .expect("a budget-limited checkpoint retains diagnostic state");
    assert_eq!(
        trace.outcome,
        AlignmentVerticalResidencyPumpOutcome::BudgetLimited
    );
    assert_eq!(trace.current_scene, visible_revision);
    assert_eq!(trace.published_scene, None);
    assert_eq!(trace.target_read_pages, None);
    assert_eq!(trace.target_summary_pages, None);
    assert!(trace.pump_elapsed_nanoseconds >= trace.prepare_elapsed_nanoseconds);
    assert_eq!(telemetry.vertical_residency_read_page_occurrences, 0);
    assert_eq!(telemetry.vertical_residency_summary_page_occurrences, 0);

    coordinator.reduce_budget(64 << 20).unwrap();
    assert!(coordinator.has_pending_presentation_work());
    for _ in 0..256 {
        if !coordinator.has_pending_presentation_work() {
            break;
        }
        let pump = coordinator.pump_presentation_once().unwrap().unwrap();
        assert!(!pump.budget_limited);
    }
    assert!(matches!(
        coordinator.frame().unwrap().vertical_residency,
        AlignmentVerticalResidency::Complete { .. }
    ));
}

#[test]
fn disjoint_camera_retarget_stops_stale_vertical_residency_at_the_next_block_boundary() {
    let (mut coordinator, camera) = test_coordinator();
    let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
        .unwrap()
        .with_full_vertical_residency(true);
    assert!(
        coordinator
            .set_preparation_policy(policy)
            .unwrap()
            .is_none()
    );
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let first = coordinator.pump_presentation_once().unwrap().unwrap();
    assert!(first.published_scene.is_none());
    let checkpoints = coordinator.telemetry().vertical_residency_checkpoints;

    let disjoint_viewport = camera
        .viewport()
        .pan_pixels(600.0, coordinator.contract().contig_length)
        .unwrap();
    let disjoint = AlignmentCamera::from_viewport(
        disjoint_viewport,
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(disjoint).unwrap();
    assert!(!update.visible_complete);
    assert!(update.foreground_submitted.is_some());
    assert_eq!(coordinator.telemetry().return_cache_demotions, 1);
    assert!(coordinator.telemetry().return_cache_entries_demoted > 0);
    assert!(!coordinator.has_pending_presentation_work());
    assert!(coordinator.pump_presentation_once().unwrap().is_none());
    assert_eq!(
        coordinator.telemetry().vertical_residency_checkpoints,
        checkpoints
    );

    pump_until_publication(&mut coordinator);
    assert!(coordinator.frame().unwrap().visible_complete());
    assert!(coordinator.has_pending_presentation_work());
}

#[test]
fn base_lod_keeps_vertical_residency_viewport_bounded() {
    assert!(!supports_full_vertical_residency(GeometryLod::Overview));
    assert!(supports_full_vertical_residency(GeometryLod::Span));
    assert!(supports_full_vertical_residency(GeometryLod::Read));
    assert!(!supports_full_vertical_residency(GeometryLod::Base));
}

#[test]
fn bottom_row_camera_clamps_to_a_non_empty_exact_range() {
    assert_eq!(clamp_visible_rows(100..120, 100), 80..100);
    assert_eq!(clamp_visible_rows(100..101, 100), 99..100);
    assert_eq!(clamp_visible_rows(95..105, 100), 90..100);
    assert_eq!(clamp_visible_rows(0..1, 1), 0..1);
    assert_eq!(clamp_visible_rows(0..1, 0), 0..0);
}

#[test]
fn an_active_gpu_replacement_is_retryable_pressure_not_a_terminal_scene_error() {
    let pressure = LedgerError::ReplacementTransitionActive { bundle_id: 17 };
    assert_eq!(budget_pressure(&pressure), Some(pressure));
}
