use super::*;

#[test]
fn frame_row_activity_is_camera_exact_while_residency_remains_atlas_wide() {
    let (mut coordinator, camera) = test_coordinator();
    let policy = coordinator
        .preparation_policy()
        .with_full_vertical_residency(true);
    assert!(
        coordinator
            .set_preparation_policy(policy)
            .unwrap()
            .is_none()
    );
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);

    let frame = coordinator.frame().unwrap();
    let snapshot = frame.snapshot.as_ref().unwrap();
    let atlas = snapshot.reads().atlas();
    let profile = frame
        .row_activity
        .as_ref()
        .expect("a fully covered read-row camera has exact activity");
    assert_eq!(profile.atlas_revision(), atlas.revision());
    assert_eq!(profile.interval(), camera.interval());
    assert_ne!(profile.interval(), atlas.interval());
    assert_eq!(profile.stable_row_count(), atlas.row_count());
    assert!(profile.bins().len() <= ALIGNMENT_ROW_ACTIVITY_BINS);
    assert!(profile.visible_record_count() > 0);
    let telemetry = coordinator.telemetry();
    assert!(telemetry.row_activity_queries > 0);
    assert!(telemetry.row_activity_rebuilds > 0);
    assert!(telemetry.row_activity_intervals_examined > 0);
    assert_eq!(
        telemetry.row_activity_queries,
        telemetry
            .row_activity_cache_hits
            .saturating_add(telemetry.row_activity_cursor_updates)
            .saturating_add(telemetry.row_activity_rebuilds)
    );

    let before_hit = telemetry;
    assert!(coordinator.frame().is_some());
    let after_hit = coordinator.telemetry();
    assert_eq!(
        after_hit.row_activity_cache_hits,
        before_hit.row_activity_cache_hits + 1
    );

    let shifted_viewport = camera
        .viewport()
        .pan_pixels(1.0, coordinator.contract().contig_length)
        .unwrap();
    let shifted_camera = AlignmentCamera::from_viewport(
        shifted_viewport,
        camera.row_scroll(),
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    coordinator.update_camera(shifted_camera).unwrap();
    let after_update = coordinator.telemetry();
    assert_eq!(
        after_update.row_activity_cursor_updates,
        after_hit.row_activity_cursor_updates + 1
    );
    assert_eq!(
        after_update.row_activity_queries,
        after_update
            .row_activity_cache_hits
            .saturating_add(after_update.row_activity_cursor_updates)
            .saturating_add(after_update.row_activity_rebuilds)
    );

    let atlas_wide_total = match frame.vertical_residency {
        AlignmentVerticalResidency::Preparing { total_rows, .. }
        | AlignmentVerticalResidency::Complete { total_rows }
        | AlignmentVerticalResidency::BudgetLimited { total_rows, .. }
        | AlignmentVerticalResidency::Unavailable { total_rows, .. } => total_rows,
        AlignmentVerticalResidency::ViewportBounded => {
            panic!("the explicitly enabled complete-residency policy must report totals")
        }
    };
    assert_eq!(
        usize::try_from(atlas_wide_total).unwrap(),
        atlas.row_count()
    );
}

#[test]
fn row_activity_authority_ignores_visible_rows_but_rejects_genomic_or_layout_change() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);

    let frame = coordinator.frame().unwrap();
    let snapshot = frame.snapshot.as_ref().unwrap();
    let profile = frame.row_activity.as_ref().unwrap();
    let vertically_scrolled = AlignmentCamera::from_viewport(
        camera.viewport(),
        camera.row_scroll() + 1_000.0,
        camera.row_height(),
        camera.read_height(),
        camera.clip(),
    )
    .unwrap();
    assert_ne!(camera.visible_rows(), vertically_scrolled.visible_rows());
    assert!(alignment_row_activity_satisfies_contract(
        profile,
        snapshot,
        vertically_scrolled.interval(),
        coordinator.contract(),
    ));

    let shifted = GenomicInterval::new(
        camera.interval().contig(),
        camera.interval().start() + 1,
        camera.interval().end() + 1,
    )
    .unwrap();
    assert!(!alignment_row_activity_satisfies_contract(
        profile,
        snapshot,
        shifted,
        coordinator.contract(),
    ));

    let mut changed_layout = coordinator.contract().clone();
    changed_layout.layout.stacking = match changed_layout.layout.stacking {
        ReadStackingMode::Stable => ReadStackingMode::Compact,
        ReadStackingMode::Compact => ReadStackingMode::Stable,
    };
    assert!(!alignment_row_activity_satisfies_contract(
        profile,
        snapshot,
        camera.interval(),
        &changed_layout,
    ));
}

#[test]
fn compatible_atlas_republication_preserves_exact_row_activity_authority() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);

    let initial = coordinator.frame().unwrap();
    let initial_scene = initial.snapshot.unwrap();
    let initial_evidence_revision = initial_scene.reads().evidence().revision();
    let initial_atlas_revision = initial_scene.reads().atlas().revision();
    let owner = coordinator.owner;
    let desired = coordinator
        .source
        .lanes
        .get(&owner)
        .unwrap()
        .desired
        .clone()
        .unwrap();

    {
        let source = &mut coordinator.source;
        let lane = source.lanes.get_mut(&owner).unwrap();
        lane.publish_source_roots_from_cache(&mut source.actor, &desired)
            .unwrap();
        lane.prepare_from_source_roots(owner, &source.actor, &mut source.presentation, &desired)
            .unwrap()
            .expect("compatible republished roots prepare a replacement scene");
    }

    let republished = coordinator.frame().unwrap();
    let republished_scene = republished.snapshot.as_ref().unwrap();
    assert_ne!(
        republished_scene.reads().evidence().revision(),
        initial_evidence_revision
    );
    assert_eq!(
        republished_scene.reads().atlas().revision(),
        initial_atlas_revision
    );
    assert_ne!(
        republished_scene.reads().atlas().derived_from_evidence(),
        republished_scene.reads().evidence().revision()
    );
    let profile = republished
        .row_activity
        .as_ref()
        .expect("compatible atlas reuse must retain exact scrollbar row authority");
    assert!(alignment_row_activity_satisfies_contract(
        profile,
        republished_scene,
        camera.interval(),
        coordinator.contract(),
    ));
}

#[test]
fn row_activity_rejects_disjoint_or_stale_snapshot_authority() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let initial = coordinator.frame().unwrap();
    let initial_snapshot = Arc::clone(initial.snapshot.as_ref().unwrap());
    let initial_profile = initial.row_activity.as_ref().unwrap();
    assert_eq!(initial_profile.interval(), camera.interval());

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
    coordinator.update_camera(disjoint).unwrap();
    let disjoint_frame = coordinator.frame().unwrap();
    assert!(Arc::ptr_eq(
        disjoint_frame.snapshot.as_ref().unwrap(),
        &initial_snapshot
    ));
    assert!(
        !initial_snapshot
            .prepared_interval()
            .contains_interval(disjoint.interval())
    );
    assert!(disjoint_frame.row_activity.is_none());

    // A cached profile is also rejected when the snapshot's real row-layout contract no
    // longer matches the lane. Restoring that exact contract produces a newly validated
    // profile for the camera rather than reusing stale authority.
    coordinator.update_camera(camera).unwrap();
    let original_layout = coordinator.contract().layout;
    coordinator
        .source
        .lanes
        .get_mut(&coordinator.owner)
        .unwrap()
        .contract
        .layout
        .stacking = match original_layout.stacking {
        ReadStackingMode::Stable => ReadStackingMode::Compact,
        ReadStackingMode::Compact => ReadStackingMode::Stable,
    };
    assert!(coordinator.frame().unwrap().row_activity.is_none());
    coordinator
        .source
        .lanes
        .get_mut(&coordinator.owner)
        .unwrap()
        .contract
        .layout = original_layout;
    let restored = coordinator.frame().unwrap();
    let restored_profile = restored.row_activity.as_ref().unwrap();
    assert_eq!(restored_profile.interval(), camera.interval());
    assert_eq!(
        restored_profile.atlas_revision(),
        restored
            .snapshot
            .as_ref()
            .unwrap()
            .reads()
            .atlas()
            .revision()
    );
}

#[test]
fn base_contract_without_bases_reports_the_exact_pending_source_horizon() {
    let revision = test_reference_revision(1);
    let (mut coordinator, camera) = base_reference_coordinator(revision);

    let update = coordinator.update_camera(camera).unwrap();
    assert!(update.foreground_submitted.is_some());
    assert!(!update.visible_complete);
    let frame = coordinator.frame().unwrap();
    let expected = GenomicInterval::new(camera.interval().contig(), 9_997, 10_003).unwrap();
    assert_eq!(
        frame.reference_requirement,
        AlignmentReferenceRequirement::Pending {
            revision,
            horizon: expected,
        }
    );
    assert!(frame.snapshot.is_none());
    assert!(!frame.scene_contracts_current);
    assert!(!frame.visible_complete());
}
