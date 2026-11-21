//! Device-free decisions of alignment scene settlement: whether an engine answer belongs to
//! the live request, how a frame settles the navigation transaction, and how pending FASTA
//! requirements of the panel's lanes are planned.

use crate::alignment::navigation::AlignmentNavigationPhase;
use rgv_alignment_scene::{AlignmentSceneCamera, ReferenceRevision};
use rgv_core::GenomicInterval;
use rgv_engine::{
    AlignmentCoordinatorFrame, AlignmentReferenceRequirement, AlignmentSceneEngineOperation,
    AlignmentSceneRequestToken, AlignmentSceneResourcePressureDisposition,
    MAX_REFERENCE_QUERY_BASES, SourceIntentKind, SourceOwnerId,
};
use std::collections::BTreeMap;

/// A FASTA completion is authoritative only for the exact open handle and request it was sent
/// for. Generation acceptance is checked separately by the owning panel.
pub fn reference_query_completion_matches(
    current_revision: Option<ReferenceRevision>,
    requested_revision: Option<ReferenceRevision>,
    requested_interval: Option<GenomicInterval>,
    completed_revision: ReferenceRevision,
    completed_interval: GenomicInterval,
) -> bool {
    current_revision == Some(completed_revision)
        && requested_revision == Some(completed_revision)
        && requested_interval == Some(completed_interval)
}

/// Whether a current FASTA request failure terminally satisfies the matching pending dependency.
/// The scene remains intact; this predicate only permits the desktop transaction to stop waiting
/// for an authority that the reference actor has explicitly failed to provide.
pub fn reference_failure_matches_requirement(
    failed_revision: ReferenceRevision,
    failed_interval: GenomicInterval,
    requirement: AlignmentReferenceRequirement,
) -> bool {
    matches!(
        requirement,
        AlignmentReferenceRequirement::Pending { revision, horizon }
            if revision == failed_revision && failed_interval.contains_interval(horizon)
    )
}

/// Shared settlement contract for interactive navigation and automation observation. A terminal
/// error settles only its exact current panel generation; otherwise the current revision, slice
/// coverage, and completion generation must all agree.
pub fn reference_dependency_is_settled(
    required: bool,
    generation: u64,
    current_revision: Option<ReferenceRevision>,
    slice_revision: Option<ReferenceRevision>,
    slice_interval: Option<GenomicInterval>,
    required_interval: GenomicInterval,
    settled_generation: Option<u64>,
    error_generation: Option<u64>,
) -> bool {
    !required
        || error_generation == Some(generation)
        || (slice_revision == current_revision
            && settled_generation == Some(generation)
            && slice_interval.is_some_and(|slice| slice.contains_interval(required_interval)))
}

/// Owner-scoped scene results are current only when both sides carry the same explicit request
/// token. In particular, `None == None` is not authority: it describes a lane with no live desktop
/// request and an unversioned error/event.
pub fn alignment_scene_event_token_is_current(
    desired: Option<AlignmentSceneRequestToken>,
    event: Option<AlignmentSceneRequestToken>,
) -> bool {
    desired.is_some() && desired == event
}

/// A published frame may update a projection only when it belongs to the exact desktop
/// transaction and has reached that transaction's camera. Contract and policy controls share the
/// request token with the following camera control, so the token alone does not make an interim
/// old-camera frame current.
pub fn alignment_scene_frame_is_current(
    desired_token: Option<AlignmentSceneRequestToken>,
    event_token: AlignmentSceneRequestToken,
    desired_camera: Option<AlignmentSceneCamera>,
    frame_camera: AlignmentSceneCamera,
) -> bool {
    alignment_scene_event_token_is_current(desired_token, Some(event_token))
        && desired_camera == Some(frame_camera)
}

/// A refusal is terminal only for the exact request camera, and its source horizon must contain
/// that camera. A token match alone is insufficient because one request token is deliberately
/// shared by the ordered contract/policy/camera controls which form one desktop transaction.
pub fn alignment_scene_refusal_is_current(
    desired_token: Option<AlignmentSceneRequestToken>,
    event_token: AlignmentSceneRequestToken,
    desired_camera: Option<AlignmentSceneCamera>,
    frame_camera: AlignmentSceneCamera,
    refused_horizon: GenomicInterval,
) -> bool {
    alignment_scene_frame_is_current(desired_token, event_token, desired_camera, frame_camera)
        && refused_horizon.contains_interval(frame_camera.interval())
}

/// A resource-pressure edge belongs to the current desktop transaction only when both its token
/// and failed source target still describe the live camera. This rejects an unread old pressure
/// notification after a latest-wins pan without requiring the UI to know the actor's sequence.
pub fn alignment_scene_pressure_is_current(
    desired_token: Option<AlignmentSceneRequestToken>,
    event_token: AlignmentSceneRequestToken,
    desired_camera: Option<AlignmentSceneCamera>,
    failed_target: GenomicInterval,
) -> bool {
    alignment_scene_event_token_is_current(desired_token, Some(event_token))
        && desired_camera.is_some_and(|camera| failed_target.contains_interval(camera.interval()))
}

/// A source-wide budget response is current only for the value both the workspace and this
/// source controller most recently submitted. This independent comparison is required because
/// `scene_budget_bytes` advances when a command is queued, before the worker proves completion.
pub fn alignment_scene_budget_event_is_current(
    workspace_budget: usize,
    submitted_source_budget: usize,
    event_budget: usize,
) -> bool {
    event_budget == workspace_budget && event_budget == submitted_source_budget
}

/// Whether a typed pressure edge must be resumed after shared-ledger progress. Optional source
/// read-ahead and explicit presentation prewarming can simply yield. Vertical presentation
/// pumping is different: the coordinator latches that lane as budget-limited until an exact
/// retry clears it, so dropping its speculative notification would permanently stop ACTB-style
/// vertical residency work.
pub fn alignment_scene_pressure_requires_resume(
    operation: AlignmentSceneEngineOperation,
    intent: SourceIntentKind,
) -> bool {
    intent != SourceIntentKind::Speculative
        || matches!(operation, AlignmentSceneEngineOperation::PumpPresentation)
}

/// Resource-pressure retries can outlive the foreground navigation transaction which started
/// them. In particular, speculative vertical row preparation continues after the exact visible
/// scene has committed. Such work must still be retried, but it must not reopen or otherwise
/// mutate an already settled navigation transaction.
pub const fn alignment_scene_pressure_owns_navigation(phase: AlignmentNavigationPhase) -> bool {
    matches!(
        phase,
        AlignmentNavigationPhase::Running | AlignmentNavigationPhase::WaitingForSourcePressure
    )
}

/// An intrinsically oversized optional row-residency page limits instant deep scrolling; it does
/// not invalidate the already complete visible scene. Keep this operation check deliberately
/// narrow: speculative source acquisition has different lifecycle semantics and continues to use
/// its existing yield/terminal handling.
pub fn alignment_scene_pressure_preserves_visible_epoch(
    operation: AlignmentSceneEngineOperation,
    intent: SourceIntentKind,
    disposition: AlignmentSceneResourcePressureDisposition,
) -> bool {
    operation == AlignmentSceneEngineOperation::PumpPresentation
        && intent == SourceIntentKind::Speculative
        && disposition == AlignmentSceneResourcePressureDisposition::Infeasible
}

/// How a current, token-matched frame affects the desktop navigation transaction. A source
/// refusal is already a terminal answer for individual reads, so it outranks a pending FASTA
/// dependency which can no longer make those reads displayable. A complete core scene settles
/// navigation even when its optional reference-backed interpretation remains pending.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentSceneFrameSettlement {
    Settled,
    SettledWithReferencePending,
    WaitingForReference,
    WaitingForScene,
}

pub fn alignment_scene_frame_settlement(
    frame: &AlignmentCoordinatorFrame,
    reference_blocks_core_scene: bool,
) -> AlignmentSceneFrameSettlement {
    if frame.individual_reads_refused_for_camera() {
        AlignmentSceneFrameSettlement::Settled
    } else if frame.visible_complete()
        && matches!(
            frame.reference_requirement,
            AlignmentReferenceRequirement::Pending { .. }
        )
    {
        AlignmentSceneFrameSettlement::SettledWithReferencePending
    } else if reference_blocks_core_scene
        && matches!(
            frame.reference_requirement,
            AlignmentReferenceRequirement::Pending { .. }
        )
    {
        // The reference is optional only after the core scene is already visible-complete. When
        // the same pending authority blocks completeness (for example an Anywhere substitution
        // filter), FASTA acquisition owns forward progress and must be scheduled explicitly.
        AlignmentSceneFrameSettlement::WaitingForReference
    } else if frame.visible_complete() {
        AlignmentSceneFrameSettlement::Settled
    } else {
        AlignmentSceneFrameSettlement::WaitingForScene
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentReferenceLaneRequirement {
    pub owner: SourceOwnerId,
    pub revision: ReferenceRevision,
    pub horizon: GenomicInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentReferenceLanePlan {
    AllQueued,
    Query {
        revision: ReferenceRevision,
        interval: GenomicInterval,
    },
}

/// Plans the next FASTA operation across every current pending alignment lane. Compatible lanes
/// share one bounded union query; incompatible or over-limit lanes are handled deterministically
/// one at a time. An owner already accepted by its engine control queue is excluded only while the
/// exact revision and horizon remain unchanged.
pub fn plan_alignment_reference_lanes(
    requirements: &[AlignmentReferenceLaneRequirement],
    queued: &BTreeMap<SourceOwnerId, (ReferenceRevision, GenomicInterval)>,
) -> AlignmentReferenceLanePlan {
    let mut outstanding = requirements
        .iter()
        .copied()
        .filter(|requirement| {
            queued.get(&requirement.owner) != Some(&(requirement.revision, requirement.horizon))
        })
        .collect::<Vec<_>>();
    outstanding.sort_unstable_by_key(|requirement| requirement.owner);
    let Some(first) = outstanding.first().copied() else {
        return AlignmentReferenceLanePlan::AllQueued;
    };
    let compatible = outstanding.iter().all(|requirement| {
        requirement.revision == first.revision
            && requirement.horizon.contig() == first.horizon.contig()
    });
    if compatible {
        let start = outstanding
            .iter()
            .map(|requirement| requirement.horizon.start())
            .min()
            .expect("an outstanding reference lane exists");
        let end = outstanding
            .iter()
            .map(|requirement| requirement.horizon.end())
            .max()
            .expect("an outstanding reference lane exists");
        if end.saturating_sub(start) <= MAX_REFERENCE_QUERY_BASES {
            let interval = GenomicInterval::new(first.horizon.contig(), start, end)
                .expect("a union of non-empty reference horizons is non-empty");
            return AlignmentReferenceLanePlan::Query {
                revision: first.revision,
                interval,
            };
        }
    }
    AlignmentReferenceLanePlan::Query {
        revision: first.revision,
        interval: first.horizon,
    }
}
