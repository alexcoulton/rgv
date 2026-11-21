use super::*;

#[test]
fn genomic_only_camera_acquires_exact_summaries_without_read_row_demand() {
    let (mut coordinator, render_camera) = test_coordinator();
    let policy = coordinator
        .preparation_policy()
        .with_full_vertical_residency(true);
    assert!(
        coordinator
            .set_preparation_policy(policy)
            .unwrap()
            .is_none()
    );
    let camera = AlignmentSceneCamera::without_visible_rows(render_camera.viewport());
    let initial = coordinator.update_camera(camera).unwrap();
    assert!(initial.foreground_submitted.is_some());
    assert!(!initial.visible_complete);

    pump_until_publication(&mut coordinator);
    let frame = coordinator.frame().unwrap();
    assert_eq!(frame.camera, camera);
    assert_eq!(frame.requested_rows, 0..0);
    assert_eq!(frame.covered_rows, 0..0);
    assert!(frame.visible_complete(), "{frame:#?}");
    assert!(frame.row_activity.is_none());
    assert_eq!(
        frame.vertical_residency,
        AlignmentVerticalResidency::ViewportBounded
    );
    assert_eq!(
        frame.individual_reads,
        AlignmentIndividualReadCapability::Pending
    );
    let snapshot = frame
        .snapshot
        .expect("summary acquisition publishes a scene");
    assert!(
        snapshot
            .summaries()
            .prepared_interval()
            .contains_interval(camera.interval())
    );
    assert!(!snapshot.summary_pages().is_empty());
    assert_eq!(snapshot.reads().prepared_rows(), 0..0);
    assert!(!coordinator.has_pending_presentation_work());
}

#[test]
fn contained_pan_and_scroll_change_only_the_live_camera() {
    let (mut coordinator, camera) = test_coordinator();
    let initial = coordinator.update_camera(camera).unwrap();
    assert!(initial.foreground_submitted.is_some());
    assert!(!initial.visible_complete);
    let revision = pump_until_publication(&mut coordinator);
    let final_frame = coordinator.frame().unwrap();
    assert!(final_frame.visible_complete(), "{final_frame:#?}");

    let before = coordinator.telemetry();
    let viewport = camera
        .viewport()
        .pan_pixels(20.0, coordinator.contract().contig_length)
        .unwrap();
    let panned = AlignmentCamera::from_viewport(
        viewport,
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(panned).unwrap();
    assert!(update.reused_current_scene);
    assert!(update.published_scene.is_none());
    assert!(update.foreground_submitted.is_none());
    assert!(update.speculation_submitted.is_none());
    assert_eq!(
        coordinator.frame().unwrap().snapshot.unwrap().revision(),
        revision
    );

    let scrolled = AlignmentCamera::from_viewport(
        viewport,
        1.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(scrolled).unwrap();
    assert!(update.reused_current_scene);
    assert!(update.published_scene.is_none());
    let after = coordinator.telemetry();
    assert_eq!(after.source_work_started, before.source_work_started);
    assert_eq!(after.evidence_candidates, before.evidence_candidates);
    assert_eq!(
        after.exact_summary_candidates,
        before.exact_summary_candidates
    );
    assert_eq!(after.row_atlas_candidates, before.row_atlas_candidates);
    assert_eq!(
        after.presentation_candidates,
        before.presentation_candidates
    );
    assert_eq!(after.scene_publications, before.scene_publications);
    assert_eq!(after.camera_only_updates, before.camera_only_updates + 2);
}

#[test]
fn pan_zoom_reversal_and_vertical_return_converge_without_new_scene_work() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    let revision = pump_until_publication(&mut coordinator);
    let contig_length = coordinator.contract().contig_length;
    let anchor = camera.clip().width * 0.5;
    let zoomed_viewport = camera
        .viewport()
        .zoom_at(anchor, 0.5, contig_length)
        .unwrap();
    let zoomed = AlignmentCamera::from_viewport(
        zoomed_viewport,
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let panned_viewport = zoomed_viewport.pan_pixels(40.0, contig_length).unwrap();
    let panned = AlignmentCamera::from_viewport(
        panned_viewport,
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let scrolled = AlignmentCamera::from_viewport(
        zoomed_viewport,
        1.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let trace = [zoomed, panned, zoomed, scrolled, zoomed, camera];
    let operations_before = coordinator
        .actor()
        .store()
        .ledger()
        .operation_counters()
        .unwrap();

    for next in trace {
        let update = coordinator.update_camera(next).unwrap();
        assert!(update.visible_complete);
        assert!(update.reused_current_scene);
        assert!(update.published_scene.is_none());
        assert!(update.foreground_submitted.is_none());
        assert!(update.speculation_submitted.is_none());
    }
    let warmed_telemetry = coordinator.telemetry();
    let warmed_ledger = coordinator.actor().store().ledger().progress().unwrap();

    for _ in 0..32 {
        for next in trace {
            let update = coordinator.update_camera(next).unwrap();
            assert!(update.visible_complete);
            assert!(update.reused_current_scene);
        }
    }
    let final_telemetry = coordinator.telemetry();
    let final_ledger = coordinator.actor().store().ledger().progress().unwrap();
    let operations_after = coordinator
        .actor()
        .store()
        .ledger()
        .operation_counters()
        .unwrap();
    assert_eq!(
        final_telemetry.source_work_started,
        warmed_telemetry.source_work_started
    );
    assert_eq!(
        final_telemetry.evidence_candidates,
        warmed_telemetry.evidence_candidates
    );
    assert_eq!(
        final_telemetry.exact_summary_candidates,
        warmed_telemetry.exact_summary_candidates
    );
    assert_eq!(
        final_telemetry.row_atlas_candidates,
        warmed_telemetry.row_atlas_candidates
    );
    assert_eq!(
        final_telemetry.presentation_candidates,
        warmed_telemetry.presentation_candidates
    );
    assert_eq!(
        final_telemetry.scene_publications,
        warmed_telemetry.scene_publications
    );
    assert_eq!(final_ledger.charged, warmed_ledger.charged);
    assert_eq!(final_ledger.retained, warmed_ledger.retained);
    assert_eq!(
        operations_after.full_inventory_snapshots, operations_before.full_inventory_snapshots,
        "camera-only recovery traces must not enumerate the allocation inventory"
    );
    assert_eq!(
        operations_after.allocation_records_enumerated,
        operations_before.allocation_records_enumerated
    );
    assert_eq!(
        coordinator.frame().unwrap().snapshot.unwrap().revision(),
        revision
    );
}

#[test]
fn outrunning_preparation_preserves_the_truthfully_covered_edge() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    let revision = pump_until_publication(&mut coordinator);
    let current_interval = coordinator
        .frame()
        .unwrap()
        .snapshot
        .as_ref()
        .unwrap()
        .prepared_interval();
    let beyond = camera
        .viewport()
        .pan_pixels(600.0, coordinator.contract().contig_length)
        .unwrap();
    let beyond = AlignmentCamera::from_viewport(
        beyond,
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    assert!(beyond.interval().overlaps(current_interval));
    assert!(!current_interval.contains_interval(beyond.interval()));

    let update = coordinator.update_camera(beyond).unwrap();
    assert!(!update.visible_complete);
    assert!(update.foreground_submitted.is_some());
    let frame = coordinator.frame().unwrap();
    assert_eq!(frame.snapshot.as_ref().unwrap().revision(), revision);
    let covered = frame
        .covered_interval
        .expect("the old immutable scene must remain visible over its truthful overlap");
    assert!(covered.overlaps(beyond.interval()));
    assert!(covered.len() > 0);
}

#[test]
fn adjacent_pans_keep_a_bounded_rolling_window_and_converge_to_a_clean_scene() {
    let (mut coordinator, initial_camera) = test_coordinator();
    coordinator.update_camera(initial_camera).unwrap();
    pump_until_publication(&mut coordinator);
    let maximum_window_bases = 6_u64;
    let initial_start = initial_camera.interval().start();
    let mut final_camera = initial_camera;

    // Each camera overlaps the preceding camera by one base. This is deliberately long
    // enough to cross the horizontal trigger repeatedly; the former overlap-union behavior
    // grew the desired interval on every crossing even though the configured buffer stayed
    // fixed at one viewport per side.
    for offset in 1..=64_u64 {
        let interval = GenomicInterval::new(
            coordinator.contract().contig,
            initial_start + offset,
            initial_start + offset + initial_camera.interval().len(),
        )
        .unwrap();
        final_camera = AlignmentCamera::new(
            interval,
            0.0,
            initial_camera.row_height(),
            initial_camera.read_height(),
            initial_camera.clip(),
        )
        .unwrap();
        coordinator.update_camera(final_camera).unwrap();

        let desired = coordinator
            .desired_prepared_interval()
            .expect("every camera update installs a preparation target");
        assert!(desired.contains_interval(final_camera.interval()));
        assert!(
            desired.len() <= maximum_window_bases,
            "fixed preparation policy grew into a corridor at offset {offset}: {desired:?}"
        );

        for _ in 0..256 {
            if !coordinator.source.has_pending_source_work() {
                break;
            }
            coordinator.pump_once().unwrap();
        }
        assert!(!coordinator.source.has_pending_source_work());

        // A speculative edge acquisition intentionally does not replace a still-visible
        // scene. Reapplying the same camera lets the now-complete cache publish the bounded
        // rolling window, exactly as the next native camera checkpoint would.
        coordinator.update_camera(final_camera).unwrap();
        let desired = coordinator.desired_prepared_interval().unwrap();
        assert!(desired.contains_interval(final_camera.interval()));
        assert!(desired.len() <= maximum_window_bases);
    }

    // The final pan lands on a trigger crossing, so the rolling scene itself—not a forced
    // reset—must match a clean load at that camera. Its authority cannot retain population
    // from any of the traversed loci.
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
}

#[test]
fn panel_capture_applies_live_pan_immediately_without_waiting_for_the_source_actor() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let source_frame = coordinator.frame().unwrap();
    let snapshot = source_frame.snapshot.unwrap();
    let before = coordinator.telemetry();
    let track = TrackId::new(17);
    let mut panel = AlignmentPanelSceneStore::new([track]);
    assert_eq!(
        panel
            .admit(
                track,
                AlignmentPanelLanePublication::new(
                    source_frame.camera_sequence,
                    Some(snapshot.clone()),
                    source_frame.scene_contracts_current,
                ),
            )
            .unwrap(),
        AlignmentPanelSceneAdmission::SceneReplaced
    );
    let initial_capture = AlignmentPanelLaneCapture::new(track, camera, Some(camera), []).unwrap();
    let initial = panel
        .capture(PanelId::new(3), 1, camera.viewport(), [initial_capture])
        .unwrap();
    assert!(initial.lanes[0].visible_complete());

    let live_viewport = camera
        .viewport()
        .pan_pixels(600.0, coordinator.contract().contig_length)
        .unwrap();
    let live_camera = AlignmentCamera::from_viewport(
        live_viewport,
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let live_capture =
        AlignmentPanelLaneCapture::new(track, live_camera, Some(live_camera), []).unwrap();
    let panned = panel
        .capture(PanelId::new(3), 2, live_camera.viewport(), [live_capture])
        .unwrap();
    assert_eq!(panned.camera_revision, 2);
    assert_eq!(panned.authority_revision, initial.authority_revision);
    assert!(Arc::ptr_eq(
        panned.lanes[0].snapshot.as_ref().unwrap(),
        &snapshot
    ));
    assert!(panned.lanes[0].covered_interval.is_some());
    assert!(!panned.lanes[0].visible_complete());
    assert_eq!(coordinator.telemetry(), before);
}

#[test]
fn panel_capture_keeps_exact_row_authority_while_vertical_pages_are_pending() {
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

    let source_frame = coordinator.frame().unwrap();
    let snapshot = Arc::clone(source_frame.snapshot.as_ref().unwrap());
    let profile = Arc::clone(source_frame.row_activity.as_ref().unwrap());
    let prepared_rows = snapshot.reads().prepared_rows();
    let total_rows = u32::try_from(snapshot.reads().atlas().row_count()).unwrap();
    assert!(prepared_rows.end < total_rows);

    let track = TrackId::new(18);
    let mut panel = AlignmentPanelSceneStore::new([track]);
    panel
        .admit(
            track,
            AlignmentPanelLanePublication::new(
                source_frame.camera_sequence,
                Some(snapshot),
                source_frame.scene_contracts_current,
            ),
        )
        .unwrap();
    let scrolled = AlignmentCamera::from_viewport(
        camera.viewport(),
        f64::from(prepared_rows.end),
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let capture = AlignmentPanelLaneCapture::new(track, scrolled, Some(scrolled), [])
        .unwrap()
        .with_row_activity(Some(Arc::clone(&profile)));
    let captured = panel
        .capture(PanelId::new(3), 2, scrolled.viewport(), [capture])
        .unwrap();
    let lane = &captured.lanes[0];

    assert!(!lane.reads_complete());
    assert!(lane.covered_rows.is_empty());
    assert!(Arc::ptr_eq(lane.row_activity.as_ref().unwrap(), &profile));
    assert_eq!(
        lane.row_activity.as_ref().unwrap().interval(),
        scrolled.interval()
    );
}

#[test]
fn edge_prefetch_is_speculative_and_later_pan_publishes_without_another_query() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let initial_scene = coordinator.frame().unwrap().snapshot.unwrap();
    let initial_work = coordinator.telemetry().source_work_started;

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
    assert!(update.visible_complete);
    assert!(update.speculation_submitted.is_some());
    assert!(update.foreground_submitted.is_none());
    assert_eq!(
        coordinator.frame().unwrap().snapshot.unwrap().revision(),
        initial_scene.revision()
    );
    pump_until_speculation_complete(&mut coordinator);
    let work_after_prefetch = coordinator.telemetry().source_work_started;
    assert_eq!(work_after_prefetch, initial_work + 1);

    let resident_viewport = camera
        .viewport()
        .pan_pixels(400.0, coordinator.contract().contig_length)
        .unwrap();
    let resident_camera = AlignmentCamera::from_viewport(
        resident_viewport,
        0.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(resident_camera).unwrap();
    assert!(update.published_scene.is_some());
    assert!(update.foreground_submitted.is_none());
    assert_eq!(
        coordinator.telemetry().source_work_started,
        work_after_prefetch
    );
    assert!(coordinator.frame().unwrap().visible_complete());

    let resident_scene = coordinator.frame().unwrap().snapshot.unwrap();
    let (mut clean, _) = test_coordinator();
    assert!(
        clean
            .set_preparation_policy(
                AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 4, 1).unwrap(),
            )
            .unwrap()
            .is_none()
    );
    let clean_camera = AlignmentCamera::new(
        resident_scene.prepared_interval(),
        0.0,
        resident_camera.row_height(),
        resident_camera.read_height(),
        resident_camera.clip(),
    )
    .unwrap();
    clean.update_camera(clean_camera).unwrap();
    pump_until_publication(&mut clean);
    let clean_scene = clean.frame().unwrap().snapshot.unwrap();
    assert_eq!(
        resident_scene.prepared_interval(),
        clean_scene.prepared_interval()
    );
    assert_eq!(
        scene_population(&resident_scene),
        scene_population(&clean_scene)
    );
    assert_eq!(
        resident_scene.reads().atlas().assignment_fingerprint(),
        clean_scene.reads().atlas().assignment_fingerprint()
    );
    assert_eq!(
        resident_scene
            .summaries()
            .project_depth_bins(resident_scene.prepared_interval(), 16)
            .unwrap(),
        clean_scene
            .summaries()
            .project_depth_bins(clean_scene.prepared_interval(), 16)
            .unwrap()
    );
}

#[test]
fn adjacent_lod_prewarm_and_zoom_switch_reuse_source_evidence() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let original = coordinator.contract().clone();
    let mut span_contract = original.clone();
    span_contract.presentation.geometry_lod = GeometryLod::Span;
    let source_work = coordinator.telemetry().source_work_started;
    let pages_before = coordinator.presentation().read_page_count();

    assert!(
        coordinator
            .prewarm_presentation(span_contract.presentation)
            .unwrap()
    );
    assert!(coordinator.presentation().read_page_count() > pages_before);
    assert_eq!(coordinator.telemetry().source_work_started, source_work);

    let update = coordinator.replace_contract(span_contract).unwrap();
    assert!(update.published_scene.is_some());
    assert!(update.foreground_submitted.is_none());
    assert!(update.speculation_submitted.is_none());
    assert_eq!(coordinator.telemetry().source_work_started, source_work);
    assert_eq!(
        coordinator
            .frame()
            .unwrap()
            .snapshot
            .unwrap()
            .reads()
            .presentation()
            .geometry_lod,
        GeometryLod::Span
    );

    let update = coordinator.replace_contract(original).unwrap();
    assert!(update.published_scene.is_some());
    assert_eq!(coordinator.telemetry().source_work_started, source_work);
    assert_eq!(
        coordinator
            .frame()
            .unwrap()
            .snapshot
            .unwrap()
            .reads()
            .presentation()
            .geometry_lod,
        GeometryLod::Read
    );
}

#[test]
fn vertical_page_extension_reuses_the_atlas_without_row_assignment_work() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let before = coordinator.telemetry();
    let atlas_revision = coordinator
        .frame()
        .unwrap()
        .snapshot
        .unwrap()
        .reads()
        .atlas()
        .revision();
    let scrolled = AlignmentCamera::from_viewport(
        camera.viewport(),
        6.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    let update = coordinator.update_camera(scrolled).unwrap();
    assert!(update.published_scene.is_some());
    assert!(update.visible_complete);
    let after = coordinator.telemetry();
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
