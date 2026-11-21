//! The `Bounded` memory policy at the coordinator: caches trim themselves, navigation is never
//! refused because of cache contents, and the one remaining pressure is reported exactly once.

use rgv_alignment_scene::{AlignmentMemoryPolicy, AllocationOwner};

use super::*;

fn bounded_ledger(budget: usize) -> AllocationLedger {
    AllocationLedger::with_policy(AlignmentMemoryPolicy::Bounded, budget, budget, 64 << 20)
}

#[test]
fn a_budget_which_refuses_the_strict_ledger_still_publishes_under_bounded() {
    // The strict ledger cannot even reserve the first decoded chunk.
    let (mut strict, camera) =
        test_coordinator_with_ledger(AllocationLedger::new_with_transient_budget(64, 0, 64 << 20));
    strict.update_camera(camera).unwrap();
    let refused = (0..256).find_map(|_| match strict.pump_once() {
        Ok(pump) => pump.pressure.map(|pressure| pressure.pressure),
        Err(error) => Some(
            source_allocation_pressure::<LocalBamCanonicalBackend>(&error)
                .expect("the strict ledger refuses with allocation pressure"),
        ),
    });
    assert!(refused.is_some(), "the strict ledger admitted the locus");

    // Bounded with a budget whose shares hold the visible scene: no refusal anywhere.
    let (mut coordinator, camera) = test_coordinator_with_ledger(bounded_ledger(64 << 20));
    coordinator.update_camera(camera).unwrap();
    let mut published = None;
    for _ in 0..256 {
        let pump = coordinator.pump_once().unwrap();
        assert!(pump.pressure.is_none());
        let trim = coordinator.trim_bounded_caches(usize::MAX).unwrap();
        assert_eq!(trim.pressure, None);
        published = published.or(pump.published_scene);
        if published.is_some() {
            break;
        }
    }
    assert!(published.is_some());
    assert!(coordinator.frame().unwrap().visible_complete());
    let progress = coordinator.actor().store().ledger().progress().unwrap();
    assert_eq!(progress.reserved, 0);
}

#[test]
fn bounded_trim_shrinks_caches_to_their_share_and_never_the_published_scene() {
    let (mut coordinator, camera) = test_coordinator_with_ledger(bounded_ledger(64 << 20));
    coordinator.update_camera(camera).unwrap();
    let revision = pump_until_publication(&mut coordinator);
    let ledger = coordinator.actor().store().ledger();

    // Inside budget the trim does nothing at all.
    let idle = coordinator.trim_bounded_caches(usize::MAX).unwrap();
    assert_eq!(
        (idle.evicted_entries, idle.complete, idle.pressure),
        (0, true, None)
    );

    let mut span = coordinator.contract().presentation;
    span.geometry_lod = GeometryLod::Span;
    assert!(coordinator.prewarm_presentation(span).unwrap());
    let pages_after_prewarm = coordinator.presentation().read_page_count();
    let before = ledger.owner_usage(AllocationOwner::Presentation).unwrap();
    assert!(before.charged > before.pinned, "the prewarm left no cache");

    // A presentation share exactly the size of what is pinned: every cached page must go.
    ledger.set_budget(before.pinned * 4).unwrap();
    let trim = coordinator.trim_bounded_caches(usize::MAX).unwrap();
    assert!(trim.evicted_entries > 0 && trim.complete);
    // Nothing is in flight, so there is no request to refuse.
    assert_eq!(trim.pressure, None);
    assert!(coordinator.presentation().read_page_count() < pages_after_prewarm);
    let after = ledger.owner_usage(AllocationOwner::Presentation).unwrap();
    assert!(
        after.charged <= after.soft_budget.max(after.pinned),
        "{after:?}"
    );
    assert_eq!(after.pinned, before.pinned);

    let frame = coordinator.frame().unwrap();
    assert_eq!(frame.snapshot.as_ref().unwrap().revision(), revision);
    assert!(frame.visible_complete());
    // A trim is a fixed point.
    assert_eq!(
        coordinator
            .trim_bounded_caches(usize::MAX)
            .unwrap()
            .evicted_entries,
        0
    );
}

#[test]
fn evidence_which_cannot_fit_its_cap_is_reported_once_and_not_decoded_again() {
    // No budget to speak of: any decoded chunk already exceeds the evidence hard cap.
    let (mut coordinator, camera) = test_coordinator_with_ledger(bounded_ledger(1));
    let update = coordinator.update_camera(camera).unwrap();
    assert!(update.foreground_submitted.is_some());
    let mut reported = None;
    for _ in 0..256 {
        let pump = coordinator.pump_once().unwrap();
        assert!(pump.pressure.is_none(), "a bounded build was refused");
        let trim = coordinator.trim_bounded_caches(usize::MAX).unwrap();
        if let Some(pressure) = trim.pressure {
            reported = Some(pressure);
            break;
        }
        assert!(
            pump.published_scene.is_none(),
            "an over-cap locus published"
        );
    }
    let pressure = reported.expect("unevictable evidence over the cap is pressure");
    assert_eq!(pressure.owner, coordinator.owner());
    assert_eq!(pressure.intent, SourceIntentKind::Foreground);
    assert!(pressure.pressure.intrinsically_exceeds_limit());

    // The request is gone and latched: the same camera neither decodes nor reports again.
    assert!(coordinator.actor().schedule().active().is_none());
    assert!(
        !coordinator
            .actor()
            .schedule()
            .foreground_pending_for(coordinator.owner())
    );
    assert!(
        coordinator
            .update_camera(camera)
            .unwrap()
            .foreground_submitted
            .is_none()
    );
    for _ in 0..16 {
        let pump = coordinator.pump_once().unwrap();
        assert!(pump.source_event.is_none());
        assert_eq!(
            coordinator
                .trim_bounded_caches(usize::MAX)
                .unwrap()
                .pressure,
            None
        );
    }
    let progress = coordinator.actor().store().ledger().progress().unwrap();
    assert_eq!((progress.reserved, progress.transient), (0, 0));

    // A larger budget is a new admission decision, and the same camera then loads.
    coordinator.reduce_budget(64 << 20).unwrap();
    assert!(
        coordinator
            .update_camera(camera)
            .unwrap()
            .foreground_submitted
            .is_some()
    );
    pump_until_publication(&mut coordinator);
    assert!(coordinator.frame().unwrap().visible_complete());
    assert_eq!(
        coordinator
            .trim_bounded_caches(usize::MAX)
            .unwrap()
            .pressure,
        None
    );
}

#[test]
fn read_ahead_yields_to_bounded_pressure_and_the_visible_scene_stays() {
    let (mut coordinator, camera) = test_coordinator_with_ledger(bounded_ledger(64 << 20));
    coordinator.update_camera(camera).unwrap();
    let revision = pump_until_publication(&mut coordinator);
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
    assert!(
        coordinator
            .update_camera(near_edge)
            .unwrap()
            .speculation_submitted
            .is_some()
    );
    assert!(matches!(
        coordinator.pump_once().unwrap().source_event,
        Some(AlignmentSourceActorEvent::WorkStarted(ref work))
            if work.kind() == SourceIntentKind::Speculative
    ));

    // The published evidence alone now exceeds its cap. Read-ahead goes; nothing is reported
    // because no visible request is being served; the scene on screen is untouched.
    coordinator.actor().store().ledger().set_budget(1).unwrap();
    let trim = coordinator.trim_bounded_caches(usize::MAX).unwrap();
    assert!(trim.speculation_aborted);
    assert_eq!(trim.pressure, None);
    assert!(coordinator.actor().schedule().active().is_none());
    assert!(
        coordinator
            .actor()
            .schedule()
            .desired_speculation()
            .is_none()
    );
    assert_eq!(
        coordinator.telemetry().resource_pressure_speculation_aborts,
        1
    );
    let frame = coordinator.frame().unwrap();
    assert_eq!(frame.snapshot.as_ref().unwrap().revision(), revision);
    assert!(frame.visible_complete());
    // The refused margin is not asked for again at this horizon.
    let update = coordinator.update_camera(near_edge).unwrap();
    assert!(update.speculation_submitted.is_none());
    assert!(
        !coordinator
            .trim_bounded_caches(usize::MAX)
            .unwrap()
            .speculation_aborted
    );
}

#[test]
fn the_strict_policy_is_untouched_by_the_bounded_trim() {
    let (mut coordinator, camera) = test_coordinator();
    coordinator.update_camera(camera).unwrap();
    pump_until_publication(&mut coordinator);
    let ledger = coordinator.actor().store().ledger();
    let before = ledger.progress().unwrap();
    ledger.set_budget(before.pinned).unwrap();
    let trim = coordinator.trim_bounded_caches(usize::MAX).unwrap();
    assert_eq!(
        (
            trim.evicted_entries,
            trim.complete,
            trim.speculation_aborted,
            trim.pressure
        ),
        (0, true, false, None)
    );
    assert_eq!(ledger.progress().unwrap().charged, before.charged);
}

#[test]
fn optional_row_warming_stops_at_the_presentation_share_and_resumes_with_a_larger_budget() {
    let (mut coordinator, camera) = test_coordinator_with_ledger(bounded_ledger(64 << 20));
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
    let ledger = coordinator.actor().store().ledger();
    // A presentation share smaller than what the visible scene already pins.
    let pinned = ledger
        .owner_usage(AllocationOwner::Presentation)
        .unwrap()
        .pinned;
    ledger.set_budget(pinned).unwrap();
    assert_eq!(
        coordinator
            .trim_bounded_caches(usize::MAX)
            .unwrap()
            .pressure,
        None
    );

    let limited = coordinator
        .pump_presentation_once()
        .unwrap()
        .expect("full vertical residency is initially pending");
    assert!(limited.budget_limited);
    assert!(limited.published_scene.is_none());
    // The latch is quiet: the refusal is intrinsic, so the driver publishes a frame, not a
    // retryable pressure event, and no further warming is attempted.
    assert!(
        limited
            .pressure
            .is_some_and(LedgerError::intrinsically_exceeds_limit)
    );
    assert!(!coordinator.has_pending_presentation_work());
    let frame = coordinator.frame().unwrap();
    assert!(frame.visible_complete());
    assert_eq!(
        frame.snapshot.as_ref().unwrap().revision(),
        visible_revision
    );
    assert!(matches!(
        frame.vertical_residency,
        AlignmentVerticalResidency::BudgetLimited { .. }
    ));
    assert_eq!(
        coordinator
            .telemetry()
            .vertical_residency_read_page_occurrences,
        0
    );

    coordinator.reduce_budget(64 << 20).unwrap();
    assert!(coordinator.has_pending_presentation_work());
    for _ in 0..256 {
        if !coordinator.has_pending_presentation_work() {
            break;
        }
        let pump = coordinator.pump_presentation_once().unwrap().unwrap();
        assert!(!pump.budget_limited);
        assert_eq!(
            coordinator
                .trim_bounded_caches(usize::MAX)
                .unwrap()
                .pressure,
            None
        );
    }
    assert!(matches!(
        coordinator.frame().unwrap().vertical_residency,
        AlignmentVerticalResidency::Complete { .. }
    ));
}
