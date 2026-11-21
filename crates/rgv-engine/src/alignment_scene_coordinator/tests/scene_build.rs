use super::*;

/// Pumps the source until its completed acquisition registers a staged build, without running
/// any stage of it.
fn pump_until_build_registered(coordinator: &mut TestCoordinator) {
    for _ in 0..256 {
        if coordinator.source.has_pending_scene_build() {
            return;
        }
        coordinator.pump_once().unwrap();
    }
    panic!("the bounded local fixture never registered a scene build");
}

fn run_build_stages(coordinator: &mut TestCoordinator, stages: usize) {
    for _ in 0..stages {
        let pump = coordinator.source.pump_scene_build_once().unwrap().unwrap();
        assert!(pump.publications.is_empty());
        assert!(pump.pressures.is_empty());
        assert!(pump.failures.is_empty());
    }
}

#[test]
fn a_staged_build_publishes_exactly_what_the_single_call_build_publishes() {
    let (mut monolithic, camera) = test_coordinator();
    monolithic.update_camera(camera).unwrap();
    assert!(pump_until_idle(&mut monolithic).is_empty());

    let (mut staged, camera) = test_coordinator();
    staged.source.set_staged_scene_builds(true);
    let update = staged.update_camera(camera).unwrap();
    assert!(update.published_scene.is_none());
    let stage_order = pump_until_idle(&mut staged);

    assert_eq!(
        stage_order,
        [
            "canonical",
            "filter",
            "summaries",
            "atlas",
            "publish",
            "pages"
        ]
    );
    assert!(!staged.source.has_pending_scene_build());
    let signature = scene_signature(&staged);
    assert!(signature.visible_complete);
    assert_eq!(signature.reserved, 0);
    assert_eq!(signature, scene_signature(&monolithic));
    let work = staged.source.owner_work_state(staged.owner()).unwrap();
    assert!(!work.source_pending && !work.presentation_pending);
}

#[test]
fn a_newer_camera_abandons_a_staged_build_before_its_next_stage_and_leaks_nothing() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.source.set_staged_scene_builds(true);
    let owner = coordinator.owner();
    coordinator.update_camera(camera).unwrap();
    pump_until_build_registered(&mut coordinator);
    let ledger = coordinator.actor().store().ledger();
    let idle = ledger.progress().unwrap();
    assert!(
        coordinator
            .source
            .owner_work_state(owner)
            .unwrap()
            .presentation_pending
    );

    // Canonical evidence, filter and exact summaries are now held by the build.
    run_build_stages(&mut coordinator, 3);
    assert_eq!(coordinator.source.scene_build_stage(owner), Some("atlas"));
    let held = ledger.progress().unwrap();
    assert!(held.pinned > idle.pinned || held.charged > idle.charged);

    // A camera somewhere else entirely arrives between two stages.
    let moved = camera_at(&coordinator, camera, camera.interval().start() + 500);
    let update = coordinator.update_camera(moved).unwrap();

    assert!(update.foreground_submitted.is_some());
    assert!(!coordinator.source.has_pending_scene_build());
    // Every pin and reservation the build held is back. What stays charged is exact-summary
    // cache which the store keeps as ordinary evictable cache, exactly as after a build which
    // completes; `abandoned_builds_leave_only_evictable_cache` proves it is reclaimable.
    let released = ledger.progress().unwrap();
    assert_eq!(released.reserved, idle.reserved);
    assert_eq!(released.pinned, idle.pinned);
    assert_eq!(released.transient, idle.transient);
    let telemetry = coordinator.telemetry();
    assert_eq!(telemetry.row_atlas_candidates, 0);
    assert_eq!(telemetry.scene_publications, 0);

    // The newer camera is then served by its own build.
    let stages = pump_until_idle(&mut coordinator);
    assert_eq!(stages.first(), Some(&"canonical"));
    let frame = coordinator.frame().unwrap();
    assert_eq!(frame.camera.interval(), moved.interval());
    assert!(frame.visible_complete());
}

/// Registers a build, runs `stages` of it, removes the lane, and reclaims every inactive
/// allocation. Returns what is still charged, pinned, and reserved.
fn residue_after_removing_a_lane_mid_build(stages: usize) -> (usize, usize, usize) {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.source.set_staged_scene_builds(true);
    let owner = coordinator.owner();
    coordinator.update_camera(camera).unwrap();
    pump_until_build_registered(&mut coordinator);
    let ledger = coordinator.actor().store().ledger();
    let idle = ledger.progress().unwrap();
    run_build_stages(&mut coordinator, stages);

    assert!(coordinator.source.remove_lane(owner));

    assert!(!coordinator.source.has_pending_scene_build());
    assert!(
        coordinator
            .source
            .pump_scene_build_once()
            .unwrap()
            .is_none()
    );
    let released = ledger.progress().unwrap();
    assert_eq!(released.reserved, idle.reserved);
    assert_eq!(released.pinned, idle.pinned);
    assert_eq!(released.transient, idle.transient);
    coordinator.source.evict_inactive_until(0).unwrap();
    let residue = ledger.progress().unwrap();
    (residue.charged, residue.pinned, residue.reserved)
}

#[test]
fn abandoned_builds_leave_only_evictable_cache() {
    // A build abandoned after any number of stages leaves exactly what a build which never
    // ran leaves: nothing the ledger cannot reclaim.
    // (The fifth stage publishes the roots as the shared source's authority, which is no longer
    // the build's own reservation.)
    let never_started = residue_after_removing_a_lane_mid_build(0);
    for stages in 1..=4 {
        assert_eq!(
            residue_after_removing_a_lane_mid_build(stages),
            never_started,
            "a build abandoned after {stages} stages leaked ledger bytes"
        );
    }
    assert_eq!(never_started.1, 0);
    assert_eq!(never_started.2, 0);
}

#[test]
fn a_contract_change_restarts_a_staged_build_from_its_first_stage() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.source.set_staged_scene_builds(true);
    let owner = coordinator.owner();
    coordinator.update_camera(camera).unwrap();
    pump_until_build_registered(&mut coordinator);
    run_build_stages(&mut coordinator, 3);
    assert_eq!(coordinator.source.scene_build_stage(owner), Some("atlas"));

    let mut contract = coordinator.contract().clone();
    contract.layout.stacking = ReadStackingMode::Stable;
    coordinator.replace_contract(contract.clone()).unwrap();

    // Products built under the old layout contract are never published.
    assert_eq!(
        coordinator.source.scene_build_stage(owner),
        Some("canonical")
    );
    pump_until_idle(&mut coordinator);
    let frame = coordinator.frame().unwrap();
    assert!(frame.visible_complete());
    assert_eq!(
        frame.snapshot.unwrap().reads().atlas().layout_contract(),
        contract.layout
    );
}

#[test]
fn a_same_zoom_pan_inside_the_interval_being_built_keeps_the_build_and_converges() {
    // The reference result: the same two cameras with every build run to completion at once.
    let (mut monolithic, camera) = test_coordinator();
    let panned = camera_at(&monolithic, camera, camera.interval().start() + 1);
    monolithic.update_camera(camera).unwrap();
    pump_until_idle(&mut monolithic);
    monolithic.update_camera(panned).unwrap();
    pump_until_idle(&mut monolithic);

    let (mut staged, camera) = test_coordinator();
    staged.source.set_staged_scene_builds(true);
    let owner = staged.owner();
    staged.update_camera(camera).unwrap();
    pump_until_build_registered(&mut staged);
    run_build_stages(&mut staged, 2);
    assert_eq!(staged.source.scene_build_stage(owner), Some("summaries"));

    // One pan sample at the same zoom, still inside the interval being built: restarting here
    // on every sample of a continuous pan would never publish anything.
    staged.update_camera(panned).unwrap();
    assert_eq!(staged.source.scene_build_stage(owner), Some("summaries"));

    pump_until_idle(&mut staged);
    assert!(!staged.source.has_pending_scene_build());
    let frame = staged.frame().unwrap();
    assert_eq!(frame.camera.interval(), panned.interval());
    assert!(frame.visible_complete());
    // The scene built for the first camera is retained for the second, exactly as when the
    // second camera arrives after that build has finished.
    assert_eq!(scene_signature(&staged), scene_signature(&monolithic));
}

/// Runs build stages until one settles the build, returning that stage's pump.
fn pump_build_until_settled(coordinator: &mut TestCoordinator) -> AlignmentSceneSourcePump {
    for _ in 0..64 {
        let pump = coordinator.source.pump_scene_build_once().unwrap().unwrap();
        if !pump.publications.is_empty() || !pump.pressures.is_empty() || !pump.failures.is_empty()
        {
            return pump;
        }
    }
    panic!("the staged build never settled");
}

#[test]
fn a_contract_change_on_cached_evidence_keeps_the_incumbent_scene_in_every_interim_frame() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.source.set_staged_scene_builds(true);
    let owner = coordinator.owner();
    coordinator.update_camera(camera).unwrap();
    pump_until_idle(&mut coordinator);
    let incumbent = coordinator.frame().unwrap().snapshot.unwrap().revision();
    let acquisitions = coordinator.telemetry().source_acquisitions;

    let mut contract = coordinator.contract().clone();
    contract.layout.stacking = ReadStackingMode::Stable;
    let update = coordinator.replace_contract(contract.clone()).unwrap();

    // The request's own frame carries no new scene. It still exposes the incumbent, marked
    // as not current, and names the worker as the owner of the replacement.
    assert!(update.published_scene.is_none());
    let mut publication = None;
    for _ in 0..64 {
        let frame = coordinator.frame().unwrap();
        assert_eq!(
            frame.snapshot.as_ref().map(|scene| scene.revision()),
            Some(incumbent),
            "the incumbent is withdrawn only by the replacement's publication"
        );
        assert!(!frame.scene_contracts_current);
        assert!(!frame.visible_complete());
        let work = coordinator.source.owner_work_state(owner).unwrap();
        assert!(work.presentation_pending && !work.source_pending);
        let pump = coordinator.source.pump_scene_build_once().unwrap().unwrap();
        assert!(pump.pressures.is_empty() && pump.failures.is_empty());
        if let Some(published) = pump.publications.first() {
            assert_eq!(published.owner, owner);
            publication = Some(published.revision);
            break;
        }
    }

    // The replacement arrives whole, in the stage which publishes it.
    let frame = coordinator.frame().unwrap();
    let scene = frame.snapshot.as_ref().unwrap();
    assert_eq!(Some(scene.revision()), publication);
    assert_ne!(scene.revision(), incumbent);
    assert!(frame.scene_contracts_current);
    assert_eq!(scene.reads().atlas().layout_contract(), contract.layout);
    pump_until_idle(&mut coordinator);
    assert!(coordinator.frame().unwrap().visible_complete());
    let work = coordinator.source.owner_work_state(owner).unwrap();
    assert!(!work.source_pending && !work.presentation_pending);
    assert_eq!(coordinator.telemetry().source_acquisitions, acquisitions);
}

#[test]
fn a_failed_build_for_a_camera_request_on_cached_evidence_is_reported_once_to_its_owner() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.source.set_staged_scene_builds(true);
    let owner = coordinator.owner();
    coordinator.update_camera(camera).unwrap();
    pump_until_idle(&mut coordinator);
    let elsewhere = camera_at(&coordinator, camera, camera.interval().start() - 9_000);
    coordinator.update_camera(elsewhere).unwrap();
    pump_until_idle(&mut coordinator);
    let acquisitions = coordinator.telemetry().source_acquisitions;

    // Returning to evidence which is still cached starts a build from the camera request.
    let update = coordinator.update_camera(camera).unwrap();
    assert!(update.published_scene.is_none());
    assert!(update.foreground_submitted.is_none());
    let work = coordinator.source.owner_work_state(owner).unwrap();
    assert!(work.presentation_pending && !work.source_pending);

    // Make the candidate impossible to build; the camera was validated against the real
    // contig length above.
    let contig_length = coordinator.contract().contig_length;
    coordinator
        .source
        .lanes
        .get_mut(&owner)
        .unwrap()
        .contract
        .contig_length = camera.interval().start();
    let pump = pump_build_until_settled(&mut coordinator);
    assert!(pump.publications.is_empty() && pump.pressures.is_empty());
    assert_eq!(pump.failures.len(), 1);
    assert_eq!(pump.failures[0].owner, owner);
    assert!(!pump.failures[0].message.is_empty());

    // Terminal for this request: nothing is left running which could report it again, so the
    // owner-scoped failure is the one event which resolves the camera request.
    assert!(!coordinator.source.has_pending_scene_build());
    assert!(
        coordinator
            .source
            .pump_scene_build_once()
            .unwrap()
            .is_none()
    );
    assert!(!coordinator.source.has_pending_source_work());
    let work = coordinator.source.owner_work_state(owner).unwrap();
    assert!(!work.source_pending && !work.presentation_pending);
    assert!(!coordinator.frame().unwrap().visible_complete());

    // A later request for the same camera tries again and converges without source I/O.
    coordinator
        .source
        .lanes
        .get_mut(&owner)
        .unwrap()
        .contract
        .contig_length = contig_length;
    coordinator.update_camera(camera).unwrap();
    assert!(coordinator.source.has_pending_scene_build());
    pump_until_idle(&mut coordinator);
    assert!(coordinator.frame().unwrap().visible_complete());
    assert_eq!(coordinator.telemetry().source_acquisitions, acquisitions);
}

#[test]
fn pressure_in_a_contract_build_is_foreground_keeps_the_incumbent_and_resumes_on_retry() {
    // No transition reserve: a replacement has only the retained budget to build in.
    let (mut coordinator, camera) = test_coordinator_with_ledger(
        AllocationLedger::new_with_transient_budget(64 << 20, 0, 64 << 20),
    );
    coordinator.source.set_staged_scene_builds(true);
    let owner = coordinator.owner();
    coordinator.update_camera(camera).unwrap();
    pump_until_idle(&mut coordinator);
    let incumbent = coordinator.frame().unwrap().snapshot.unwrap().revision();
    let ledger = coordinator.actor().store().ledger();
    let roomy_budget = ledger.snapshot().unwrap().budget;
    ledger
        .set_budget(ledger.snapshot().unwrap().charged)
        .unwrap();

    let mut contract = coordinator.contract().clone();
    contract.layout.stacking = ReadStackingMode::Stable;
    coordinator.replace_contract(contract).unwrap();
    let pump = pump_build_until_settled(&mut coordinator);
    assert!(pump.publications.is_empty() && pump.failures.is_empty());
    assert_eq!(pump.pressures.len(), 1);
    let pressure = &pump.pressures[0];
    assert_eq!(pressure.owner, owner);
    // The build served visible work, and its target contains the camera the desktop is
    // waiting on, so the desktop treats the pressure as current and resumable.
    assert_eq!(pressure.intent, SourceIntentKind::Foreground);
    assert!(pressure.target.contains_interval(camera.interval()));
    let target = pressure.target;

    // The refused build is gone, not respun, and the incumbent is still the frame's scene.
    assert!(!coordinator.source.has_pending_scene_build());
    assert!(
        coordinator
            .source
            .pump_scene_build_once()
            .unwrap()
            .is_none()
    );
    let frame = coordinator.frame().unwrap();
    assert_eq!(
        frame.snapshot.as_ref().map(|scene| scene.revision()),
        Some(incumbent)
    );
    assert!(!frame.visible_complete());

    ledger.set_budget(roomy_budget).unwrap();
    let update = coordinator
        .source
        .retry_resource_pressure(owner, target)
        .unwrap()
        .expect("the pressure target is still the lane's source horizon");
    assert!(update.published_scene.is_none());
    assert!(coordinator.source.has_pending_scene_build());
    pump_until_idle(&mut coordinator);
    assert!(coordinator.frame().unwrap().visible_complete());
}
