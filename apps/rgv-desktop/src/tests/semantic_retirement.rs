//! Retiring a presented alignment epoch must work under both memory policies.
//!
//! Regression: under the Bounded policy the GPU ledger is private, but the semantic (CPU scene)
//! component of an epoch's retirement was planned against the GPU ledger, so every replacement
//! of a presented scene failed with "retirement reference for allocation … belongs to another
//! ledger" and the panel could not present the new scene.

use super::scene_fixture::alignment_scene_snapshot_on;
use crate::workspace::alignment_management::{
    alignment_gpu_ledger_for, plan_alignment_semantic_retirement,
};
use rgv_alignment_scene::{
    AlignmentMemoryPolicy, AllocationBundleError, AllocationLedger,
    build_alignment_semantic_retirement_claim_set,
};

const TOTAL: usize = 64 << 20;

#[test]
fn bounded_policy_retires_a_presented_scene_whose_cpu_allocations_live_on_the_scene_ledger() {
    let scene = AllocationLedger::with_policy(AlignmentMemoryPolicy::Bounded, TOTAL, TOTAL, TOTAL);
    let gpu = alignment_gpu_ledger_for(&scene, TOTAL);
    let snapshot = alignment_scene_snapshot_on(&scene);
    let scene_charged = scene.progress().unwrap().charged;
    assert!(scene_charged > 0, "the fixture charges its CPU scene to the scene ledger");
    assert_eq!(gpu.progress().unwrap().charged, 0);

    // The defect: the scene's leases are foreign to the private GPU ledger.
    assert!(matches!(
        build_alignment_semantic_retirement_claim_set(&gpu, [&snapshot]),
        Err(AllocationBundleError::ForeignRetirementReference { .. })
    ));

    // The planner proves ownership on the scene ledger and hands the GPU bundle an empty claim
    // that the GPU ledger accepts.
    let (claim, stats) = plan_alignment_semantic_retirement(&gpu, &scene, true, [&snapshot])
        .expect("semantic retirement plans under the bounded policy");
    assert!(claim.is_empty());
    assert_eq!(stats.unique_snapshots, 1);
    assert_eq!(stats.exclusively_owned_snapshots, 1);
    assert!(stats.claimed_allocations > 0, "ownership is still proven");
    gpu.combine_retirement_claim_sets([&claim])
        .expect("the GPU bundle accepts the semantic component");

    // Retiring the epoch drops its snapshots; the scene ledger releases their bytes itself.
    drop(snapshot);
    assert_eq!(scene.progress().unwrap().charged, 0);
}

#[test]
fn complete_policy_still_credits_the_semantic_bytes_to_the_shared_ledger() {
    let scene =
        AllocationLedger::with_policy(AlignmentMemoryPolicy::Complete, TOTAL, TOTAL, TOTAL);
    let gpu = alignment_gpu_ledger_for(&scene, TOTAL);
    let snapshot = alignment_scene_snapshot_on(&scene);

    let (claim, stats) = plan_alignment_semantic_retirement(&gpu, &scene, false, [&snapshot])
        .expect("semantic retirement plans under the complete policy");
    assert!(!claim.is_empty(), "one ledger: the old epoch's bytes are credited");
    assert_eq!(claim.len(), stats.claimed_allocations);
    gpu.combine_retirement_claim_sets([&claim])
        .expect("the shared ledger accepts the semantic component");
}
