use crate::workspace::alignment_management::gpu_publication::{
    AlignmentGpuPublicationRetryPoll, alignment_capacity_reclaim_allows_retry,
    alignment_gpu_bundle_blocker, alignment_gpu_pressure_target_charged,
    alignment_gpu_publication_blocker_remains, alignment_gpu_publication_retry_gate,
    alignment_gpu_publication_retry_poll, alignment_read_replacement_blocker,
    alignment_summary_replacement_blocker, begin_alignment_gpu_publication_attempt,
};
use crate::workspace::alignment_management::scene_authority::retire_reference_install_receipt_for_contract_epoch;
use crate::{
    AlignmentGpuPublicationBlocker, AlignmentGpuPublicationRetry, AlignmentGpuPublicationRetryGate,
    AlignmentScenePublicationAttempt, AlignmentScenePublicationAuthority,
    AlignmentSceneTargetIdentity,
};
use rgv_alignment_scene::{
    AllocationBundleError, AllocationBundleSpec, LedgerError, LedgerProgressEpoch,
    ReferenceRevision,
};
use rgv_alignment_scene::{AllocationLedger, RevisionFingerprint};
use rgv_core::{ContigId, GenomicInterval};
use rgv_engine::SourceOwnerId;
use rgv_render::{
    AlignmentGpuReplacementError, AlignmentSummaryGpuReplacementError, GpuBudgetPressure,
};
use std::collections::BTreeMap;

#[test]
fn contract_replacement_retires_only_its_reference_install_receipt() {
    let first = SourceOwnerId::new(7);
    let second = SourceOwnerId::new(8);
    let revision = ReferenceRevision::new(RevisionFingerprint::new(1, 2));
    let interval = GenomicInterval::new(ContigId::new(0), 10, 20).unwrap();
    let mut receipts = BTreeMap::from([
        (first, (revision, interval)),
        (second, (revision, interval)),
    ]);

    assert!(retire_reference_install_receipt_for_contract_epoch(
        &mut receipts,
        first,
    ));
    assert!(!receipts.contains_key(&first));
    assert_eq!(receipts.get(&second), Some(&(revision, interval)));
    assert!(!retire_reference_install_receipt_for_contract_epoch(
        &mut receipts,
        first,
    ));
}

fn target(workspace_layout_revision: u64) -> AlignmentSceneTargetIdentity {
    AlignmentSceneTargetIdentity {
        workspace_layout_revision,
        surface_size: [1_200, 800],
        scale_factor: 2.0,
        panels: Vec::new(),
    }
}

fn waiting_retry(
    attempt: &AlignmentScenePublicationAttempt,
    epoch: LedgerProgressEpoch,
) -> AlignmentGpuPublicationRetry {
    AlignmentGpuPublicationRetry {
        target_revision: attempt.target_revision,
        attempt_sequence: attempt.sequence,
        blocker: AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
            bundle_id: 41,
        }),
        gate: AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(epoch),
    }
}

fn assert_waiting(poll: AlignmentGpuPublicationRetryPoll) -> AlignmentGpuPublicationRetry {
    match poll {
        AlignmentGpuPublicationRetryPoll::Waiting(retry) => retry,
        other => panic!("expected a waiting retry, got {other:?}"),
    }
}

fn active_replacement(
    ledger: &AllocationLedger,
) -> (rgv_alignment_scene::AllocationBundleReservation, u64) {
    let snapshot = ledger.snapshot().unwrap();
    let reservation = ledger
        .reserve_replacement_bundle(AllocationBundleSpec::new(
            snapshot.policy_revision,
            Vec::new(),
            ledger.empty_retirement_claim_set(),
        ))
        .unwrap();
    let bundle_id = ledger
        .snapshot()
        .unwrap()
        .active_replacement_bundle
        .unwrap();
    (reservation, bundle_id)
}

fn retained_allocation(
    ledger: &AllocationLedger,
    bytes: usize,
) -> rgv_alignment_scene::AllocationLease {
    let mut reservation = ledger
        .reserve(
            rgv_alignment_scene::AllocationClass::GpuPage,
            bytes,
            rgv_alignment_scene::ReservationKind::Normal,
        )
        .unwrap();
    reservation.register(bytes, 1).unwrap()
}

#[test]
fn cache_ownership_handoff_permits_one_retry_without_faking_released_bytes() {
    let ledger = AllocationLedger::new(1_000, 500);
    let _retained = retained_allocation(&ledger, 900);
    let snapshot = ledger.progress().unwrap();
    let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::BudgetExceeded {
        requested: 200,
        charged: 900,
        reserved: 0,
        limit: 1_000,
    });
    assert!(alignment_gpu_publication_blocker_remains(
        blocker, &snapshot
    ));
    assert!(!alignment_capacity_reclaim_allows_retry(
        blocker, &snapshot, 0
    ));
    assert!(alignment_capacity_reclaim_allows_retry(
        blocker, &snapshot, 1
    ));
}

#[test]
fn steady_replacement_retry_preserves_exact_retirement_credit() {
    let ledger = AllocationLedger::new(1_000, 500);
    let retained = retained_allocation(&ledger, 900);
    let releasable = retained_allocation(&ledger, 100);
    ledger.set_limits(800, 500, 800).unwrap();
    let pressure = GpuBudgetPressure::from_steady_replacement_failure(500, 1_000, 600, 800);
    let blocker = AlignmentGpuPublicationBlocker::AggregateGpu(pressure);

    let refused = ledger.progress().unwrap();
    assert!(alignment_gpu_publication_blocker_remains(blocker, &refused));
    // The exact fit point is 900 bytes: 600 bytes of the old epoch retire with the successor.
    // Reclaim deliberately continues to a 400-byte low-water mark, leaving one additional
    // measured 500-byte foreground bundle of headroom after this replacement.
    assert_eq!(
        alignment_gpu_pressure_target_charged(pressure, &refused),
        400
    );

    drop(releasable);
    let fitting = ledger.progress().unwrap();
    assert_eq!(fitting.charged, 900);
    assert!(!alignment_gpu_publication_blocker_remains(
        blocker, &fitting
    ));
    assert_eq!(
        alignment_gpu_pressure_target_charged(pressure, &fitting),
        400
    );
    drop(retained);
}

#[test]
fn unchanged_ledger_epoch_never_turns_a_refusal_into_a_frame_rate_retry() {
    let ledger = AllocationLedger::new(1_024, 128);
    let (_reservation, bundle_id) = active_replacement(&ledger);
    let snapshot = ledger.snapshot().unwrap();
    let blocker =
        AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
            bundle_id,
        });
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &snapshot),
    };

    assert_eq!(
        assert_waiting(alignment_gpu_publication_retry_poll(
            retry, 7, 11, &snapshot
        )),
        retry
    );
    assert_eq!(
        assert_waiting(alignment_gpu_publication_retry_poll(
            retry,
            7,
            11,
            &ledger.snapshot().unwrap()
        )),
        retry
    );
}

#[test]
fn same_target_refresh_cannot_bypass_or_invalidate_a_waiting_attempt() {
    let ledger = AllocationLedger::new(1_024, 128);
    let snapshot = ledger.snapshot().unwrap();
    let epoch = snapshot.progress_epoch();
    let target = target(1);
    let mut authority = AlignmentScenePublicationAuthority::default();
    let mut retry = None;
    let deferred_attempt =
        begin_alignment_gpu_publication_attempt(&mut authority, &mut retry, &target, &snapshot)
            .unwrap();
    retry = Some(waiting_retry(&deferred_attempt, epoch));
    let retained = retry;

    assert!(
        begin_alignment_gpu_publication_attempt(&mut authority, &mut retry, &target, &snapshot,)
            .is_none()
    );
    assert_eq!(retry, retained);
    assert_eq!(authority.attempt_sequence(), deferred_attempt.sequence);
    assert_eq!(
        authority.desired_target_revision(),
        deferred_attempt.target_revision
    );
}

#[test]
fn changed_target_supersedes_a_waiting_attempt_without_ledger_progress() {
    let ledger = AllocationLedger::new(1_024, 128);
    let snapshot = ledger.snapshot().unwrap();
    let epoch = snapshot.progress_epoch();
    let first_target = target(1);
    let second_target = target(2);
    let mut authority = AlignmentScenePublicationAuthority::default();
    let mut retry = None;
    let deferred_attempt = begin_alignment_gpu_publication_attempt(
        &mut authority,
        &mut retry,
        &first_target,
        &snapshot,
    )
    .unwrap();
    retry = Some(waiting_retry(&deferred_attempt, epoch));

    let changed = begin_alignment_gpu_publication_attempt(
        &mut authority,
        &mut retry,
        &second_target,
        &snapshot,
    )
    .unwrap();
    assert!(retry.is_none());
    assert_eq!(changed.sequence, deferred_attempt.sequence + 1);
    assert!(authority.desired_target_matches(&second_target));
    assert_ne!(changed.target_revision, deferred_attempt.target_revision);
}

#[test]
fn same_target_retry_starts_once_after_genuine_ledger_progress() {
    let ledger = AllocationLedger::new(1_024, 128);
    let refused = ledger.snapshot().unwrap();
    let refused_epoch = refused.progress_epoch();
    let target = target(1);
    let mut authority = AlignmentScenePublicationAuthority::default();
    let mut retry = None;
    let deferred_attempt =
        begin_alignment_gpu_publication_attempt(&mut authority, &mut retry, &target, &refused)
            .unwrap();
    retry = Some(waiting_retry(&deferred_attempt, refused_epoch));
    ledger.set_budget(2_048).unwrap();

    let retried = begin_alignment_gpu_publication_attempt(
        &mut authority,
        &mut retry,
        &target,
        &ledger.snapshot().unwrap(),
    )
    .unwrap();
    assert!(retry.is_none());
    assert_eq!(retried.sequence, deferred_attempt.sequence + 1);
    assert_eq!(retried.target_revision, deferred_attempt.target_revision);
}

#[test]
fn replacement_settlement_advances_progress_and_wakes_exactly_the_current_attempt() {
    let ledger = AllocationLedger::new(1_024, 128);
    let (reservation, bundle_id) = active_replacement(&ledger);
    let refused = ledger.snapshot().unwrap();
    let blocker =
        AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
            bundle_id,
        });
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &refused),
    };

    drop(reservation);
    let settled = ledger.snapshot().unwrap();
    assert_eq!(
        alignment_gpu_publication_retry_poll(retry, 7, 11, &settled),
        AlignmentGpuPublicationRetryPoll::Ready
    );
    assert_eq!(
        alignment_gpu_publication_retry_poll(retry, 8, 11, &settled),
        AlignmentGpuPublicationRetryPoll::Superseded
    );
    assert_eq!(
        alignment_gpu_publication_retry_poll(retry, 7, 12, &settled),
        AlignmentGpuPublicationRetryPoll::Superseded
    );
}

#[test]
fn unrelated_pin_churn_cannot_bypass_the_same_active_replacement_bundle() {
    let ledger = AllocationLedger::new(1_024, 128);
    let lease = retained_allocation(&ledger, 64);
    let (reservation, bundle_id) = active_replacement(&ledger);
    let refused = ledger.snapshot().unwrap();
    let blocker =
        AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
            bundle_id,
        });
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &refused),
    };

    let pin = lease.pin().unwrap();
    let churned = ledger.snapshot().unwrap();
    assert_ne!(churned.progress_epoch(), refused.progress_epoch());
    let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &churned));
    assert_eq!(
        updated.gate,
        AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(churned.progress_epoch())
    );

    drop(reservation);
    let settled = ledger.snapshot().unwrap();
    assert_eq!(
        alignment_gpu_publication_retry_poll(updated, 7, 11, &settled),
        AlignmentGpuPublicationRetryPoll::Ready
    );
    drop(pin);
}

#[test]
fn already_settled_replacement_race_is_ready_without_waiting_for_another_epoch() {
    let ledger = AllocationLedger::new(1_024, 128);
    let (reservation, bundle_id) = active_replacement(&ledger);
    drop(reservation);
    let post_error = ledger.snapshot().unwrap();
    let blocker =
        AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
            bundle_id,
        });

    assert_eq!(
        alignment_gpu_publication_retry_gate(blocker, &post_error),
        AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
    );
}

#[test]
fn transient_release_advances_progress_before_retrying_the_current_attempt() {
    let ledger = AllocationLedger::new_with_transient_budget(1_024, 128, 1_024);
    let transient = ledger
        .reserve_transient(
            rgv_alignment_scene::TransientResourceClass::GpuUpload,
            1_000,
            rgv_alignment_scene::ReservationKind::Normal,
        )
        .unwrap();
    let refused = ledger.snapshot().unwrap();
    let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::TransientBudgetExceeded {
        class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
        requested: 100,
        transient: 1_000,
        limit: 1_024,
    });
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &refused),
    };
    assert_eq!(
        assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &refused)),
        retry
    );

    drop(transient);
    assert_eq!(
        alignment_gpu_publication_retry_poll(retry, 7, 11, &ledger.snapshot().unwrap()),
        AlignmentGpuPublicationRetryPoll::Ready
    );
}

#[test]
fn partial_transient_release_remains_waiting_until_the_request_fits() {
    let ledger = AllocationLedger::new_with_transient_budget(1_024, 128, 1_024);
    let large = ledger
        .reserve_transient(
            rgv_alignment_scene::TransientResourceClass::GpuUpload,
            900,
            rgv_alignment_scene::ReservationKind::Normal,
        )
        .unwrap();
    let small = ledger
        .reserve_transient(
            rgv_alignment_scene::TransientResourceClass::GpuUpload,
            100,
            rgv_alignment_scene::ReservationKind::Normal,
        )
        .unwrap();
    let refused = ledger.snapshot().unwrap();
    let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::TransientBudgetExceeded {
        class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
        requested: 200,
        transient: 1_000,
        limit: 1_024,
    });
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &refused),
    };

    drop(small);
    let partial = ledger.snapshot().unwrap();
    let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &partial));
    assert_eq!(
        updated.gate,
        AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(partial.progress_epoch())
    );

    drop(large);
    assert_eq!(
        alignment_gpu_publication_retry_poll(updated, 7, 11, &ledger.snapshot().unwrap()),
        AlignmentGpuPublicationRetryPoll::Ready
    );
}

#[test]
fn unrelated_pin_churn_keeps_exact_retained_pressure_waiting() {
    let ledger = AllocationLedger::new(1_100, 128);
    let lease = retained_allocation(&ledger, 1_000);
    let refused = ledger.snapshot().unwrap();
    let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::BudgetExceeded {
        requested: 200,
        charged: 1_000,
        reserved: 0,
        limit: 1_100,
    });
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &refused),
    };

    let pin = lease.pin().unwrap();
    let churned = ledger.snapshot().unwrap();
    assert_ne!(churned.progress_epoch(), refused.progress_epoch());
    let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &churned));
    assert_eq!(
        updated.gate,
        AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(churned.progress_epoch())
    );
    drop(pin);
}

#[test]
fn unrelated_charge_churn_keeps_a_retained_refusal_waiting_until_it_fits() {
    let ledger = AllocationLedger::new(1_500, 128);
    let original = retained_allocation(&ledger, 1_000);
    let refused = ledger.snapshot().unwrap();
    let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::BudgetExceeded {
        requested: 600,
        charged: 1_000,
        reserved: 0,
        limit: 1_500,
    });
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &refused),
    };

    let unrelated = retained_allocation(&ledger, 100);
    let churned = ledger.snapshot().unwrap();
    let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &churned));
    assert_eq!(
        updated.gate,
        AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(churned.progress_epoch())
    );

    drop(unrelated);
    drop(original);
    let fitting = ledger.snapshot().unwrap();
    assert_eq!(
        alignment_gpu_publication_retry_poll(updated, 7, 11, &fitting),
        AlignmentGpuPublicationRetryPoll::Ready
    );
}

#[test]
fn unrelated_pin_churn_keeps_exact_transient_pressure_waiting() {
    let ledger = AllocationLedger::new_with_transient_budget(2_048, 128, 1_024);
    let lease = retained_allocation(&ledger, 64);
    let transient = ledger
        .reserve_transient(
            rgv_alignment_scene::TransientResourceClass::GpuUpload,
            1_000,
            rgv_alignment_scene::ReservationKind::Normal,
        )
        .unwrap();
    let refused = ledger.snapshot().unwrap();
    let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::TransientBudgetExceeded {
        class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
        requested: 100,
        transient: 1_000,
        limit: 1_024,
    });
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &refused),
    };

    let pin = lease.pin().unwrap();
    let churned = ledger.snapshot().unwrap();
    assert_ne!(churned.progress_epoch(), refused.progress_epoch());
    let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &churned));
    assert_eq!(
        updated.gate,
        AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(churned.progress_epoch())
    );
    drop(pin);
    drop(transient);
}

#[test]
fn raced_budget_policy_or_occupancy_change_is_observed_before_retry_recording() {
    let ledger = AllocationLedger::new(2_048, 128);
    let post_error = ledger.snapshot().unwrap();
    let old_policy = AlignmentGpuPublicationBlocker::Ledger(LedgerError::BudgetExceeded {
        requested: 100,
        charged: 1_000,
        reserved: 0,
        limit: 1_024,
    });
    let old_occupancy =
        AlignmentGpuPublicationBlocker::Ledger(LedgerError::TransientBudgetExceeded {
            class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
            requested: 100,
            transient: 1_000,
            limit: 1_024,
        });

    assert_eq!(
        alignment_gpu_publication_retry_gate(old_policy, &post_error),
        AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
    );
    assert_eq!(
        alignment_gpu_publication_retry_gate(old_occupancy, &post_error),
        AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
    );
}

#[test]
fn allocation_policy_revision_race_is_typed_and_ready_to_replan_once() {
    let ledger = AllocationLedger::new(1_024, 128);
    ledger.set_budget(2_048).unwrap();
    let snapshot = ledger.snapshot().unwrap();
    let error = AllocationBundleError::PolicyRevisionChanged {
        planned: snapshot.policy_revision - 1,
        current: snapshot.policy_revision,
    };
    let blocker = alignment_gpu_bundle_blocker(&error).unwrap();
    assert!(matches!(
        blocker,
        AlignmentGpuPublicationBlocker::PolicyRevisionChanged { .. }
    ));
    assert_eq!(
        alignment_gpu_publication_retry_gate(blocker, &snapshot),
        AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
    );
    let retry = AlignmentGpuPublicationRetry {
        target_revision: 7,
        attempt_sequence: 11,
        blocker,
        gate: alignment_gpu_publication_retry_gate(blocker, &snapshot),
    };
    assert_eq!(
        alignment_gpu_publication_retry_poll(retry, 7, 11, &snapshot),
        AlignmentGpuPublicationRetryPoll::Ready
    );

    // Renderer-plan/reservation disagreement cannot be caused by a later ledger edit: both
    // values are already immutable tokens at this point, so it remains an invariant failure.
    assert!(
        alignment_read_replacement_blocker(&AlignmentGpuReplacementError::BundlePolicyRevision {
            planned: 1,
            reserved: 2,
        })
        .is_none()
    );
    assert!(
        alignment_summary_replacement_blocker(
            &AlignmentSummaryGpuReplacementError::BundlePolicyRevision {
                planned: 1,
                reserved: 2,
            }
        )
        .is_none()
    );

    // If either renderer surfaces the actual allocation-bundle race, it retains the typed
    // recoverable classification through the wrapper.
    assert!(matches!(
        alignment_read_replacement_blocker(&AlignmentGpuReplacementError::Bundle(error)),
        Some(AlignmentGpuPublicationBlocker::PolicyRevisionChanged { .. })
    ));
    assert!(matches!(
        alignment_summary_replacement_blocker(&AlignmentSummaryGpuReplacementError::Bundle(error)),
        Some(AlignmentGpuPublicationBlocker::PolicyRevisionChanged { .. })
    ));
}
