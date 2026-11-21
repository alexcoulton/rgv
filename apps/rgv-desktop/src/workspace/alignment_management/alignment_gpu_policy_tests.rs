use crate::workspace::alignment_management::gpu_publication::{
    AlignmentGpuReclaimOutcome, alignment_gpu_bundle_budget_pressure,
};
use crate::{
    AlignmentGpuPrefetchState, AlignmentGpuPreparedAuthority, AlignmentGpuSpeculationGate,
    AlignmentGpuSpeculationPhase,
};
use std::{sync::Arc, time::Duration};

use rgv_alignment_scene::{
    AlignmentPageFamily, AlignmentPageKey, AlignmentSourceRevision, CanonicalGenomicTile,
    CanonicalRowBlock, ContractFingerprint, EvidenceRevision, GeometryLod, RevisionFingerprint,
    RowAtlasRevision, SourceFingerprint,
};
use rgv_core::ContigId;
use rgv_render::{GpuBudgetPressure, GpuReclaimGoal};

use rgv_alignment_scene::AllocationBundleError;

fn authority(tile: u64) -> AlignmentGpuPreparedAuthority {
    let source = AlignmentSourceRevision::new(
        SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
        RevisionFingerprint::new(3, 4),
    );
    let key = AlignmentPageKey::new(
        source,
        EvidenceRevision::new(RevisionFingerprint::new(5, 0)),
        RowAtlasRevision::new(RevisionFingerprint::new(6, 0)),
        ContractFingerprint::new(7),
        ContractFingerprint::new(8),
        CanonicalGenomicTile::new(
            ContigId::new(0),
            tile,
            std::num::NonZeroU64::new(100).unwrap(),
        ),
        CanonicalRowBlock::new(0, std::num::NonZeroU32::new(64).unwrap()),
        AlignmentPageFamily::ReadGeometry,
        GeometryLod::Read,
    );
    AlignmentGpuPreparedAuthority {
        read_pages: Arc::from([key]),
        summary_pages: Arc::from([]),
    }
}

#[test]
fn pressure_suppression_converges_until_authority_or_budget_increase_changes() {
    let mut gate = AlignmentGpuSpeculationGate::default();
    gate.observe_foreground(authority(0), 1_000);
    assert!(gate.allows_prefetch());
    gate.suppress_for_foreground_pressure();
    for _ in 0..10 {
        gate.observe_foreground(authority(0), 1_000);
        assert!(!gate.allows_prefetch());
        assert_eq!(gate.phase, AlignmentGpuSpeculationPhase::Suppressed);
    }

    gate.observe_foreground(authority(1), 1_000);
    assert!(gate.allows_prefetch());
    gate.suppress_for_foreground_pressure();
    gate.observe_foreground(authority(1), 2_000);
    assert!(gate.allows_prefetch());
}

#[test]
fn lowering_budget_suppresses_instead_of_rewarming() {
    let mut gate = AlignmentGpuSpeculationGate::default();
    gate.observe_foreground(authority(0), 2_000);
    gate.finish_cycle(true, false);
    assert_eq!(gate.phase, AlignmentGpuSpeculationPhase::Warming);
    gate.observe_foreground(authority(0), 1_000);
    assert_eq!(gate.phase, AlignmentGpuSpeculationPhase::Suppressed);
    assert!(!gate.allows_prefetch());
}

#[test]
fn reclaim_outcome_distinguishes_retry_infeasible_and_pinned_shortfall() {
    let pressure = GpuBudgetPressure::from_budget_failure(200, 900, 0, 1_000);
    let goal = GpuReclaimGoal::new(pressure, 900);
    assert_eq!(
        AlignmentGpuReclaimOutcome::for_goal(goal, 800, 500, 1, 2),
        AlignmentGpuReclaimOutcome::Satisfied {
            required_bytes: 100,
            released_bytes: 100,
            retired_read_residencies: 1,
            retired_summary_residencies: 2,
        }
    );
    assert_eq!(
        AlignmentGpuReclaimOutcome::for_goal(goal, 850, 700, 1, 2),
        AlignmentGpuReclaimOutcome::Insufficient {
            required_bytes: 100,
            released_bytes: 50,
            remaining_bytes: 50,
            pinned_bytes: 700,
        }
    );

    let oversized = GpuReclaimGoal::new(
        GpuBudgetPressure::from_budget_failure(1_100, 0, 0, 1_000),
        0,
    );
    assert_eq!(
        AlignmentGpuReclaimOutcome::for_goal(oversized, 0, 0, 0, 0),
        AlignmentGpuReclaimOutcome::Infeasible {
            required_bytes: 100,
            baseline_occupied_bytes: 0,
        }
    );
}

#[test]
fn adaptive_gpu_headroom_never_demands_reclaim_below_the_pinned_floor() {
    let pressure = GpuBudgetPressure::from_budget_failure(200, 900, 0, 1_000);
    let goal = GpuReclaimGoal::new(pressure, 900).with_additional_headroom_above_floor(200, 750);
    assert_eq!(goal.required_reclaim_bytes(), 150);
    assert!(goal.is_satisfied_at(750));

    let pinned_at_exact_fit =
        GpuReclaimGoal::new(pressure, 900).with_additional_headroom_above_floor(200, 800);
    assert_eq!(pinned_at_exact_fit.required_reclaim_bytes(), 100);
    assert!(pinned_at_exact_fit.is_satisfied_at(800));
}

#[test]
fn intrinsically_oversized_steady_candidate_retains_typed_capacity_pressure() {
    for (occupied, retiring) in [(128, 64), (4_096, 3_500)] {
        let error = AllocationBundleError::SteadyBudgetExceeded {
            candidate: 1_001,
            occupied,
            retiring,
            limit: 1_000,
        };
        let pressure = alignment_gpu_bundle_budget_pressure(&error)
            .expect("an oversized exact candidate remains typed capacity pressure");
        assert_eq!(
            pressure.cause,
            rgv_render::GpuBudgetPressureCause::SteadyReplacement
        );
        assert_eq!(pressure.requested_bytes, 1_001);
        assert_eq!(pressure.retirement_credit_bytes, retiring);
        assert!(!GpuReclaimGoal::new(pressure, occupied.saturating_sub(retiring)).is_feasible());
    }

    let overlap = alignment_gpu_bundle_budget_pressure(
        &AllocationBundleError::TransitionOverlapBudgetExceeded {
            candidate: 500,
            occupied: 900,
            limit: 1_200,
        },
    )
    .unwrap();
    assert_eq!(
        overlap.cause,
        rgv_render::GpuBudgetPressureCause::TransitionOverlap
    );
    assert_eq!(overlap.retirement_credit_bytes, 0);
}

#[test]
fn optional_prefetch_quantum_adapts_gradually_from_measured_wall_time() {
    let mut state = AlignmentGpuPrefetchState::default();
    assert_eq!(state.maximum_pages().get(), 1);
    state.observe_quantum(Duration::from_micros(100), 1);
    assert_eq!(state.maximum_pages().get(), 2);
    state.observe_quantum(Duration::from_micros(100), 2);
    assert_eq!(state.maximum_pages().get(), 4);
    state.observe_quantum(Duration::from_millis(100), 4);
    assert_eq!(state.maximum_pages().get(), 2);
}

mod memory_policy {
    use crate::workspace::alignment_management::gpu_budget::{
        alignment_gpu_ledger_for, alignment_gpu_share_bytes,
    };
    use rgv_alignment_scene::{
        AlignmentMemoryPolicy, AllocationClass, AllocationLedger, LedgerError, ReservationKind,
    };
    use rgv_core::DesktopSettings;

    const TOTAL: usize = 64 << 20;

    #[test]
    fn the_default_policy_is_bounded_and_the_strict_policy_is_one_setting_away() {
        assert_eq!(
            DesktopSettings::default().alignment_memory_policy,
            AlignmentMemoryPolicy::Bounded
        );
        // A settings file written before the field existed loads as the default.
        let legacy = DesktopSettings::from_json(
            r#"{"schema_version":1,"recent_alignments":[],"recent_sessions":[],"recent_loci":[],"cache_budget_mib":512,"diagnostics_opt_in":false}"#,
        )
        .unwrap();
        assert_eq!(
            legacy.alignment_memory_policy,
            AlignmentMemoryPolicy::Bounded
        );
        let strict = DesktopSettings {
            alignment_memory_policy: AlignmentMemoryPolicy::Complete,
            ..DesktopSettings::default()
        };
        let json = strict.to_json().unwrap();
        assert!(
            json.contains(r#""alignment_memory_policy": "complete""#),
            "{json}"
        );
        assert_eq!(
            DesktopSettings::from_json(&json)
                .unwrap()
                .alignment_memory_policy,
            AlignmentMemoryPolicy::Complete
        );
    }

    #[test]
    fn the_strict_policy_charges_gpu_pages_to_the_shared_ledger_as_before() {
        let scene =
            AllocationLedger::with_policy(AlignmentMemoryPolicy::Complete, TOTAL, TOTAL, TOTAL);
        let gpu = alignment_gpu_ledger_for(&scene, TOTAL);
        let page = gpu
            .reserve(AllocationClass::GpuPage, 4_096, ReservationKind::Normal)
            .unwrap()
            .register(4_096, 1)
            .unwrap();
        // One authority: the GPU charge is the scene ledger's charge.
        assert_eq!(scene.progress().unwrap().charged, 4_096);
        assert_eq!(gpu.progress().unwrap().budget, TOTAL);
        drop(page);
        assert_eq!(scene.progress().unwrap().charged, 0);
    }

    #[test]
    fn the_bounded_policy_gives_gpu_pages_a_private_share_which_cpu_work_cannot_touch() {
        let scene =
            AllocationLedger::with_policy(AlignmentMemoryPolicy::Bounded, TOTAL, TOTAL, TOTAL);
        let gpu = alignment_gpu_ledger_for(&scene, TOTAL);
        let share = alignment_gpu_share_bytes(TOTAL);
        assert_eq!(share, TOTAL / 4);
        let progress = gpu.progress().unwrap();
        assert_eq!(progress.policy, AlignmentMemoryPolicy::Complete);
        assert_eq!(
            (progress.budget, progress.transition_reserve),
            (share, share)
        );

        // CPU scene content far beyond the whole budget leaves the GPU share untouched...
        let evidence = scene
            .reserve(AllocationClass::CoreChunk, 0, ReservationKind::Normal)
            .unwrap()
            .register(TOTAL * 2, 1)
            .unwrap();
        let page = gpu
            .reserve(AllocationClass::GpuPage, share, ReservationKind::Normal)
            .unwrap()
            .register(share, 1)
            .unwrap();
        assert_eq!(gpu.progress().unwrap().charged, share);
        assert_eq!(scene.progress().unwrap().charged, TOTAL * 2);
        // ...and a full GPU share refuses only the GPU, with its own numbers.
        assert!(matches!(
            gpu.reserve(AllocationClass::GpuPage, 1, ReservationKind::Normal),
            Err(LedgerError::BudgetExceeded { requested: 1, limit, .. }) if limit == share
        ));
        assert!(
            scene
                .reserve(AllocationClass::SemanticPage, 1, ReservationKind::Normal)
                .is_ok()
        );
        drop((evidence, page));
        assert_eq!(gpu.progress().unwrap().charged, 0);
        assert_eq!(scene.progress().unwrap().charged, 0);
    }
}
