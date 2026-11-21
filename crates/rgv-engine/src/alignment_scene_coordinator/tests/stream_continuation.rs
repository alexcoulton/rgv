use super::*;

/// The reads which overlap the frame's camera, by stable handle.
fn visible_population(coordinator: &TestCoordinator) -> Vec<rgv_alignment_scene::AlignmentHandle> {
    let frame = coordinator.frame().unwrap();
    let scene = frame.snapshot.as_ref().expect("a published scene");
    let mut handles = scene
        .reads()
        .evidence()
        .select(frame.camera.interval())
        .unwrap()
        .records()
        .iter()
        .map(|record| record.handle)
        .collect::<Vec<_>>();
    handles.sort_unstable();
    handles
}

/// Submits `camera` and runs exactly one source checkpoint, which starts its decoder stream.
fn start_stream(coordinator: &mut TestCoordinator, camera: AlignmentCamera) {
    let update = coordinator.update_camera(camera).unwrap();
    assert!(update.foreground_submitted.is_some());
    assert!(matches!(
        coordinator.pump_once().unwrap().source_event,
        Some(AlignmentSourceActorEvent::WorkStarted(_))
    ));
}

#[test]
fn overlapping_camera_updates_over_uncached_data_keep_one_decoder_stream() {
    const UPDATES: u64 = 32;
    let (mut coordinator, camera) = test_coordinator();
    let nudged = camera_at(&coordinator, camera, camera.interval().start() + 1);
    start_stream(&mut coordinator, camera);

    // Every sample overlaps the one before it and arrives while the decoder is still busy.
    // Each used to submit a new intent, and so restart the stream, because the camera interval
    // (the admission target) differed.
    for step in 0..UPDATES {
        let sample = if step % 2 == 0 { nudged } else { camera };
        let update = coordinator.update_camera(sample).unwrap();
        assert_eq!(update.foreground_submitted, None);
        assert!(coordinator.source.has_pending_foreground_source_work());
    }
    pump_until_idle(&mut coordinator);

    let telemetry = coordinator.telemetry();
    assert_eq!(telemetry.camera_updates, UPDATES + 1);
    assert_eq!(telemetry.foreground_submissions, 1);
    assert_eq!(telemetry.source_work_started, 1);
    assert_eq!(telemetry.source_retargets, 0);
    assert_eq!(telemetry.source_yields, 0);
    assert_eq!(telemetry.source_acquisitions, 1);

    // The sequence ends on `camera`: the result is the scene one request for it builds.
    let (mut single, camera) = test_coordinator();
    single.update_camera(camera).unwrap();
    pump_until_idle(&mut single);
    assert_eq!(
        coordinator.frame().unwrap().camera.interval(),
        camera.interval()
    );
    assert_eq!(scene_signature(&coordinator), scene_signature(&single));
}

#[test]
fn a_pan_inside_the_window_being_acquired_is_served_by_the_stream_already_running() {
    let (mut coordinator, camera) = test_coordinator();
    let panned = camera_at(&coordinator, camera, camera.interval().start() + 1);
    start_stream(&mut coordinator, camera);
    let acquiring = coordinator.desired_prepared_interval().unwrap();

    let update = coordinator.update_camera(panned).unwrap();

    // The window being decoded still holds the camera with its trigger margin, so it stays
    // the target instead of being replaced by a window shifted by one base.
    assert_eq!(update.foreground_submitted, None);
    assert_eq!(coordinator.desired_prepared_interval(), Some(acquiring));
    pump_until_idle(&mut coordinator);
    let telemetry = coordinator.telemetry();
    assert_eq!(telemetry.foreground_submissions, 1);
    assert_eq!(telemetry.source_work_started, 1);
    assert_eq!(telemetry.source_retargets, 0);

    // What the final camera shows is exactly what one request for that camera shows.
    let (mut single, _) = test_coordinator();
    single.update_camera(panned).unwrap();
    pump_until_idle(&mut single);
    let frame = coordinator.frame().unwrap();
    let single_frame = single.frame().unwrap();
    assert_eq!(frame.camera.interval(), panned.interval());
    assert!(frame.visible_complete() && single_frame.visible_complete());
    assert_eq!(
        visible_population(&coordinator),
        visible_population(&single)
    );
    assert_eq!(
        frame.row_activity.unwrap().visible_record_count(),
        single_frame.row_activity.unwrap().visible_record_count()
    );
}

#[test]
fn a_camera_which_leaves_the_window_being_acquired_still_replaces_the_stream() {
    let (mut coordinator, camera) = test_coordinator();
    start_stream(&mut coordinator, camera);
    let acquiring = coordinator.desired_prepared_interval().unwrap();

    // Latest-wins is unchanged for a camera the running stream cannot serve.
    let far = camera_at(&coordinator, camera, camera.interval().start() + 500);
    let update = coordinator.update_camera(far).unwrap();

    assert!(update.foreground_submitted.is_some());
    assert_ne!(coordinator.desired_prepared_interval(), Some(acquiring));
    pump_until_idle(&mut coordinator);
    let frame = coordinator.frame().unwrap();
    assert_eq!(frame.camera.interval(), far.interval());
    assert!(frame.visible_complete());
    assert_eq!(coordinator.telemetry().foreground_submissions, 2);
}

#[test]
fn a_refusal_counted_for_an_older_camera_is_asked_again_for_the_live_camera_once() {
    let (mut coordinator, dense) = test_coordinator();
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
    let moved = camera_at(&coordinator, dense, dense.interval().start() + 1);
    start_stream(&mut coordinator, dense);

    // The stream keeps running for the moved camera, but it is counting `dense`.
    assert_eq!(
        coordinator
            .update_camera(moved)
            .unwrap()
            .foreground_submitted,
        None
    );

    let mut refusals = Vec::new();
    let mut resubmitted = 0;
    for _ in 0..256 {
        if !coordinator.source.has_pending_source_work() {
            break;
        }
        let pump = coordinator.pump_once().unwrap();
        refusals.extend(pump.refusal);
        resubmitted += usize::from(pump.foreground_submitted.is_some());
    }

    // The count for the camera nobody is looking at is not reported as the answer; the live
    // camera gets its own exact count from exactly one further stream.
    assert_eq!(resubmitted, 1);
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].target, moved.interval());
    let telemetry = coordinator.telemetry();
    assert_eq!(telemetry.foreground_submissions, 2);
    assert_eq!(telemetry.source_refusals, 2);
    assert!(
        coordinator
            .frame()
            .unwrap()
            .individual_reads_refused_for_camera()
    );
    assert!(!coordinator.source.has_pending_source_work());
}

#[test]
fn a_pan_outside_the_covered_window_decodes_only_what_is_not_resident() {
    const START: u64 = 800_000;
    let (mut coordinator, camera, log) = wide_coordinator(START);
    coordinator.update_camera(camera).unwrap();
    pump_until_idle(&mut coordinator);
    assert!(coordinator.frame().unwrap().visible_complete());
    let covered = coordinator.desired_prepared_interval().unwrap();
    let streams_before = log.streams();
    let resident = streams_before
        .iter()
        .flat_map(crate::local_bam_source::test_support::DecodedStream::offsets)
        .collect::<BTreeSet<_>>();
    assert!(
        resident.len() > 8,
        "the first window must span several BGZF blocks, not {}",
        resident.len()
    );

    // Far enough that the camera leaves the covered window, near enough that the new
    // acquisition overlaps evidence which is still resident.
    let panned = camera_at(
        &coordinator,
        camera,
        START + camera.interval().len() + camera.interval().len() / 2,
    );
    assert!(!covered.contains_interval(panned.interval()));
    let update = coordinator.update_camera(panned).unwrap();
    assert!(update.foreground_submitted.is_some());
    pump_until_idle(&mut coordinator);
    let frame = coordinator.frame().unwrap();
    assert_eq!(frame.camera.interval(), panned.interval());
    assert!(frame.visible_complete());

    // One fresh request for the panned camera: what it decodes and what it shows.
    let (mut single, _, single_log) = wide_coordinator(panned.interval().start());
    single.update_camera(panned).unwrap();
    pump_until_idle(&mut single);
    let fresh_offsets = single_log
        .streams()
        .iter()
        .flat_map(crate::local_bam_source::test_support::DecodedStream::offsets)
        .collect::<BTreeSet<_>>();

    let after_pan = &log.streams()[streams_before.len()..];
    assert!(!after_pan.is_empty());
    let decoded_after_pan = after_pan
        .iter()
        .flat_map(crate::local_bam_source::test_support::DecodedStream::offsets)
        .collect::<BTreeSet<_>>();
    let honoured = after_pan
        .iter()
        .map(|stream| stream.receipts_honoured)
        .sum::<usize>();
    let needed_and_resident = fresh_offsets.intersection(&resident).count();
    assert!(
        needed_and_resident > 4,
        "the pan must need blocks which are already resident"
    );
    assert!(honoured > 0, "no resident chunk was skipped");
    // Of the resident blocks the panned camera needs, at most the one at the old right edge
    // is read again per stream; everything else decoded is new.
    let reread = decoded_after_pan.intersection(&resident).count();
    assert!(
        reread <= after_pan.len(),
        "{reread} resident blocks were decoded again by {} streams",
        after_pan.len()
    );
    assert!(decoded_after_pan.len() < fresh_offsets.len());

    // The final scene is the one a single fresh request builds.
    let single_frame = single.frame().unwrap();
    assert!(single_frame.visible_complete());
    assert_eq!(single_frame.camera.interval(), panned.interval());
    let scene = frame.snapshot.as_ref().unwrap();
    let single_scene = single_frame.snapshot.as_ref().unwrap();
    assert_eq!(scene.prepared_interval(), single_scene.prepared_interval());
    assert!(!scene_population(scene).is_empty());
    assert_eq!(scene_population(scene), scene_population(single_scene));
    assert_eq!(
        visible_population(&coordinator),
        visible_population(&single)
    );
    assert_eq!(
        scene.reads().atlas().row_count(),
        single_scene.reads().atlas().row_count()
    );
    assert_eq!(
        scene.reads().prepared_rows(),
        single_scene.reads().prepared_rows()
    );
    assert_eq!(
        scene.reads().pages().len(),
        single_scene.reads().pages().len()
    );
    assert_eq!(
        scene.summary_pages().len(),
        single_scene.summary_pages().len()
    );
    assert_eq!(
        frame.row_activity.unwrap().visible_record_count(),
        single_frame.row_activity.unwrap().visible_record_count()
    );
}
