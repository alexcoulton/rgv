//! One alignment track as the session holds it: its scene and coverage engines, filters,
//! per-panel projections and navigation transactions, and the pure decisions that turn the
//! track's settings into scene contracts and query options.

use crate::alignment::navigation::{AlignmentNavigationPhase, AlignmentNavigationTransaction};
use crate::sources::{alignment_open_options, primary_alignment_track};
use anyhow::Result;
use rgv_alignment_scene::{
    AlignmentCoherenceGroupId, AlignmentCoverageLaneOptions, AlignmentFilterPredicate,
    AlignmentFilterScope, AlignmentHandle, AlignmentInteractionState,
    AlignmentModificationLaneOptions, AlignmentReadLabelMode, AlignmentSashimiLaneOptions,
    AlignmentSashimiScalePolicy, AlignmentSceneCamera, AlignmentStyleContract,
    AlignmentViewFilterContract, AlleleSummaryDetail, AllocationLedger, CapabilityRequirement,
    ContractFingerprint, CoverageAlleleInterpretation, CpuAlleleProjectionOptions,
    CpuDepthProjectionOptions, CpuJunctionProjectionOptions, CpuModificationProjectionOptions,
    GeometryLod, LayoutContract, LedgerError, LedgerProgress, LedgerProgressEpoch, LedgerSnapshot,
    MAXIMUM_HIDDEN_MODIFICATION_CODES, PayloadContract, PresentationContract, ReadSampling,
    ReferenceRevision, SummaryContract,
};
use rgv_core::{
    AlignmentAuxComparison, AlignmentAuxFilterValue, AlignmentPredicateScope,
    AlignmentReadSampling, AlignmentViewPredicate, ContigAliases, ContigId, DetailLevel,
    GenerationClock, GenomicInterval, MAXIMUM_BASE_MARK_BASES_PER_PIXEL, PanelId,
    ReferenceDictionary, SashimiCountMode, SashimiScaleMode, SessionAlignmentTrack,
    SessionDocument, SessionFilters, TrackId, TrackSource, Viewport, ViewportGeneration,
};
use rgv_engine::{
    AlignmentCoordinatorFrame, AlignmentSceneBuildContract, AlignmentSceneCoordinatorTelemetry,
    AlignmentSceneEngine, AlignmentScenePreparationPolicy, AlignmentSceneRequestToken,
    AlignmentSceneSourceMetadata, LocalAlignmentSceneEngineConfig, PositionedAlignmentBackend,
    SourceOwnerId,
};
use rgv_hts::{
    AlignmentFilter, AuxComparison, AuxPredicate, FilterValue, ModificationCode, QueryDetail,
    QueryOptions, TemplateLengthBounds,
};
use rgv_layout::{AlignmentColorMode, AlignmentGroupMode, AlignmentSortMode};
use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Canonical presentation partition used by the first desktop qualification slice. This is a
/// cache/page granularity, never an evidence or display cap. Measurements may replace it without
/// changing scene identity or completeness semantics.
const DESKTOP_ALIGNMENT_SCENE_TILE_BASES: u64 = 32_768;
const DESKTOP_ALIGNMENT_SCENE_ROW_BLOCK_ROWS: u32 = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlignmentReferenceCompatibility {
    Compatible,
    Incompatible(String),
}

/// Compares the immutable dictionaries before a FASTA revision is admitted into an alignment
/// scene. Extra decoy/alternate contigs are allowed, but every shared contig must retain the same
/// length and at least one contig must resolve. Name matching honours configured aliases and the
/// conventional `chr` spelling difference; neither can hide an assembly-length mismatch.
pub fn alignment_reference_compatibility(
    alignment: &ReferenceDictionary,
    reference: &ReferenceDictionary,
    aliases: &ContigAliases,
) -> AlignmentReferenceCompatibility {
    let mut matched = 0_usize;
    for alignment_contig in alignment.iter() {
        let Some(reference_contig) =
            matching_reference_contig(alignment_contig.name(), reference, aliases)
        else {
            continue;
        };
        matched = matched.saturating_add(1);
        if alignment_contig.length() != reference_contig.length() {
            return AlignmentReferenceCompatibility::Incompatible(format!(
                "contig {} is {} bp in the alignment but {} is {} bp in the FASTA",
                alignment_contig.name(),
                alignment_contig.length(),
                reference_contig.name(),
                reference_contig.length(),
            ));
        }
    }
    if matched == 0 {
        AlignmentReferenceCompatibility::Incompatible(
            "the alignment and FASTA dictionaries have no resolvable contigs in common".to_owned(),
        )
    } else {
        AlignmentReferenceCompatibility::Compatible
    }
}

/// Returns the FASTA spelling for one alignment contig only after the dictionaries are globally
/// compatible and this exact contig has the same length.
pub fn compatible_reference_contig_name(
    alignment: &ReferenceDictionary,
    reference: &ReferenceDictionary,
    aliases: &ContigAliases,
    alignment_name: &str,
) -> Result<String, String> {
    if let AlignmentReferenceCompatibility::Incompatible(reason) =
        alignment_reference_compatibility(alignment, reference, aliases)
    {
        return Err(reason);
    }
    let alignment_contig = alignment
        .id_by_name(alignment_name)
        .and_then(|id| alignment.get(id))
        .ok_or_else(|| format!("alignment dictionary has no contig named {alignment_name}"))?;
    let reference_contig = matching_reference_contig(alignment_name, reference, aliases)
        .ok_or_else(|| format!("FASTA dictionary has no contig matching {alignment_name}"))?;
    if alignment_contig.length() != reference_contig.length() {
        return Err(format!(
            "contig {} is {} bp in the alignment but {} is {} bp in the FASTA",
            alignment_contig.name(),
            alignment_contig.length(),
            reference_contig.name(),
            reference_contig.length(),
        ));
    }
    Ok(reference_contig.name().to_owned())
}

fn matching_reference_contig<'a>(
    alignment_name: &str,
    reference: &'a ReferenceDictionary,
    aliases: &ContigAliases,
) -> Option<&'a rgv_core::Contig> {
    let mut candidates = Vec::with_capacity(5);
    candidates.push(alignment_name.to_owned());
    if let Some(canonical) = aliases.get(alignment_name) {
        candidates.push(canonical.clone());
    }
    candidates.extend(
        aliases
            .iter()
            .filter(|(_, canonical)| canonical.as_str() == alignment_name)
            .map(|(alias, _)| alias.clone()),
    );
    if let Some(stripped) = alignment_name.strip_prefix("chr") {
        candidates.push(stripped.to_owned());
        if stripped == "M" {
            candidates.push("MT".to_owned());
        }
    } else {
        candidates.push(format!("chr{alignment_name}"));
        if alignment_name == "MT" {
            candidates.push("chrM".to_owned());
        }
    }
    candidates.into_iter().find_map(|candidate| {
        reference
            .id_by_name(&candidate)
            .and_then(|id| reference.get(id))
    })
}

/// Panel-local coherence identities. Raw and CPM scales are deliberately different groups: a
/// numerical maximum in one unit can never be applied to the other.
pub const ALIGNMENT_SASHIMI_RAW_GROUP: AlignmentCoherenceGroupId =
    AlignmentCoherenceGroupId::new(1);
pub const ALIGNMENT_SASHIMI_CPM_GROUP: AlignmentCoherenceGroupId =
    AlignmentCoherenceGroupId::new(2);

pub const fn alignment_scene_sashimi_group(
    filters: &SessionFilters,
) -> Option<AlignmentCoherenceGroupId> {
    if !matches!(filters.sashimi_scale_mode, SashimiScaleMode::Shared) {
        return None;
    }
    Some(match filters.sashimi_count_mode {
        SashimiCountMode::Raw => ALIGNMENT_SASHIMI_RAW_GROUP,
        SashimiCountMode::CountsPerMillion => ALIGNMENT_SASHIMI_CPM_GROUP,
    })
}

/// Captures a panel selection only into the lane that owns it. Coverage and modification lanes
/// deliberately share this gate so neither can leak another track's mutable selection into an
/// immutable panel epoch.
pub fn alignment_scene_selected_position(
    selected_track: Option<TrackId>,
    track_id: TrackId,
    position: Option<u64>,
) -> Option<u64> {
    (selected_track == Some(track_id))
        .then_some(position)
        .flatten()
}

pub fn expanded_workspace_alignment_limit(
    current_workspace_limit: usize,
    requested_track_limit: usize,
) -> (usize, usize) {
    let requested_track_limit =
        requested_track_limit.clamp(1, rgv_core::MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT);
    (
        current_workspace_limit.max(requested_track_limit),
        requested_track_limit,
    )
}

/// Translates mutable session settings into the immutable coverage contract captured by one panel
/// epoch. Allele painting is present only when the snapshot has an exact reference-backed allele
/// authority; coverage depth itself remains reference independent.
pub fn alignment_scene_coverage_lane_options(
    filters: &SessionFilters,
    reference_backed_alleles: bool,
    selected_position: Option<u64>,
) -> AlignmentCoverageLaneOptions {
    let scale_maximum = filters.coverage_scale_maximum;
    let transform = filters.coverage_scale_transform;
    AlignmentCoverageLaneOptions {
        depth: CpuDepthProjectionOptions {
            scale_maximum,
            transform,
            show_center_line: filters.show_coverage_center_line,
            selected_position,
        },
        alleles: reference_backed_alleles.then_some(CpuAlleleProjectionOptions {
            minimum_fraction: f64::from(filters.minimum_allele_fraction),
            quality_weighted: filters.quality_weighted_alleles,
            scale_maximum,
            transform,
            interpretation: match filters.alignment_color_mode {
                AlignmentColorMode::Bisulfite(context) => {
                    CoverageAlleleInterpretation::Bisulfite(context)
                }
                _ => CoverageAlleleInterpretation::GenericReferenceAware,
            },
        }),
    }
}

/// Captures Sashimi scale policy rather than prematurely resolving a shared maximum from a
/// different frame. Shared values are resolved only after the complete coherence group has
/// entered the same panel epoch.
pub fn alignment_scene_sashimi_lane_options(
    filters: &SessionFilters,
    shared_group: Option<AlignmentCoherenceGroupId>,
) -> Option<AlignmentSashimiLaneOptions> {
    let scale = match filters.sashimi_scale_mode {
        SashimiScaleMode::Automatic => AlignmentSashimiScalePolicy::Automatic,
        SashimiScaleMode::Fixed => AlignmentSashimiScalePolicy::Fixed(f64::from(
            filters.sashimi_fixed_scale_maximum.max(1),
        )),
        SashimiScaleMode::Shared => AlignmentSashimiScalePolicy::Shared(shared_group?),
    };
    let scale_maximum = match scale {
        AlignmentSashimiScalePolicy::Fixed(maximum) => Some(maximum),
        AlignmentSashimiScalePolicy::Automatic | AlignmentSashimiScalePolicy::Shared(_) => None,
    };
    Some(AlignmentSashimiLaneOptions {
        projection: CpuJunctionProjectionOptions {
            minimum_depth: filters.minimum_junction_depth,
            minimum_flank: filters.minimum_junction_flank,
            display_mode: filters.junction_display_mode,
            scale_maximum,
            count_mode: filters.sashimi_count_mode,
            show_labels: filters.show_junction_labels,
        },
        scale,
    })
}

pub fn alignment_scene_modification_lane_options(
    filters: &SessionFilters,
    selected_position: Option<u64>,
) -> AlignmentModificationLaneOptions {
    let mut hidden_codes = filters
        .hidden_modification_codes
        .iter()
        .filter_map(|value| parse_modification_code(value))
        .collect::<Vec<_>>();
    hidden_codes.sort_unstable();
    hidden_codes.dedup();
    AlignmentModificationLaneOptions {
        projection: CpuModificationProjectionOptions {
            probability_weighted: true,
            hidden_codes,
        },
        color_mode: filters.modified_base_color_mode,
        selected_position,
    }
}

#[derive(Clone)]
pub struct FilterEditor {
    pub minimum_mapq: u8,
    pub include_secondary: bool,
    pub include_supplementary: bool,
    pub expression: String,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct ActiveFilters {
    pub alignment: AlignmentFilter,
    pub include_secondary: bool,
    pub include_supplementary: bool,
}

/// Canonical alignment state used only while no alignment controller exists.
/// Once a source is open, the first controller owns the same state directly.
pub struct EmptyAlignmentState {
    pub filters: SessionFilters,
    pub filter_editor: FilterEditor,
    pub active_filters: ActiveFilters,
    pub name: String,
    pub visible: bool,
    pub collapsed: bool,
    pub maximum_alignments: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AlignmentTrackLifecycle {
    Opening,
    Ready,
    Failed(String),
}

/// Whether the exact retained/transient obstruction reported by a ledger error is still present.
/// The progress epoch is deliberately not sufficient: unrelated pins, allocations, or another
/// policy lane can change that epoch without making this request admissible.
pub trait AlignmentLedgerProgressView {
    fn alignment_ledger_progress(&self) -> LedgerProgress;
}

impl AlignmentLedgerProgressView for LedgerProgress {
    fn alignment_ledger_progress(&self) -> LedgerProgress {
        *self
    }
}

impl AlignmentLedgerProgressView for LedgerSnapshot {
    fn alignment_ledger_progress(&self) -> LedgerProgress {
        self.capacity_progress()
    }
}

pub fn alignment_ledger_pressure_blocker_remains(
    pressure: LedgerError,
    snapshot: &impl AlignmentLedgerProgressView,
) -> bool {
    let snapshot = snapshot.alignment_ledger_progress();
    match pressure {
        LedgerError::ReplacementTransitionActive { bundle_id } => {
            snapshot.active_replacement_bundle == Some(bundle_id)
        }
        LedgerError::BudgetExceeded {
            requested, limit, ..
        } => {
            let same_policy_lane = limit == snapshot.budget
                || limit == snapshot.budget.saturating_add(snapshot.transition_reserve);
            same_policy_lane
                && snapshot
                    .charged
                    .saturating_add(snapshot.reserved)
                    .saturating_add(requested)
                    > limit
        }
        LedgerError::TransientBudgetExceeded {
            requested, limit, ..
        } => {
            let same_policy_lane = limit == snapshot.transient_budget
                || limit
                    == snapshot
                        .transient_budget
                        .saturating_add(snapshot.transition_reserve);
            same_policy_lane && snapshot.transient.saturating_add(requested) > limit
        }
        LedgerError::ReservationExceeded { .. }
        | LedgerError::UnknownAllocation(_)
        | LedgerError::IdOverflow
        | LedgerError::Poisoned => false,
    }
}

/// One attempted resume of an exact pressure-paused source request. The epoch is only a cheap
/// change detector; after it changes the exact typed obstruction must also be revalidated before
/// another request is allowed into the engine mailbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentScenePressureRetry {
    pub request_token: AlignmentSceneRequestToken,
    pub target: GenomicInterval,
    pub pressure: LedgerError,
    pub ledger_epoch: LedgerProgressEpoch,
    /// True after the exact resume entered the engine mailbox and until either a current frame or
    /// another typed pressure event answers it.
    pub in_flight: bool,
}

impl AlignmentScenePressureRetry {
    pub fn same_request(
        self,
        request_token: AlignmentSceneRequestToken,
        target: GenomicInterval,
    ) -> bool {
        self.request_token == request_token && self.target == target
    }

    pub fn poll(
        mut self,
        snapshot: &impl AlignmentLedgerProgressView,
    ) -> AlignmentScenePressureRetryPoll {
        let snapshot = snapshot.alignment_ledger_progress();
        if self.in_flight || self.ledger_epoch == snapshot.progress_epoch() {
            return AlignmentScenePressureRetryPoll::Waiting(self);
        }
        if alignment_ledger_pressure_blocker_remains(self.pressure, &snapshot) {
            // Consume unrelated epoch churn once, without reopening the exact source request on
            // every subsequent desktop poll.
            self.ledger_epoch = snapshot.progress_epoch();
            AlignmentScenePressureRetryPoll::Waiting(self)
        } else {
            AlignmentScenePressureRetryPoll::Ready
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentScenePressureRetryPoll {
    Waiting(AlignmentScenePressureRetry),
    Ready,
}

/// Classifies the first post-relief snapshot without manufacturing an engine retry. Callers store
/// the returned waiting authority verbatim; `Ready` alone permits one mailbox submission.
pub fn alignment_scene_pressure_after_relief(
    request_token: AlignmentSceneRequestToken,
    target: GenomicInterval,
    pressure: LedgerError,
    snapshot: &impl AlignmentLedgerProgressView,
) -> AlignmentScenePressureRetryPoll {
    let snapshot = snapshot.alignment_ledger_progress();
    if alignment_ledger_pressure_blocker_remains(pressure, &snapshot) {
        AlignmentScenePressureRetryPoll::Waiting(AlignmentScenePressureRetry {
            request_token,
            target,
            pressure,
            ledger_epoch: snapshot.progress_epoch(),
            in_flight: false,
        })
    } else {
        AlignmentScenePressureRetryPoll::Ready
    }
}

/// Independent retry authority for one source coordinator's application-wide budget edit. The
/// controller records the requested value as soon as it queues the original command, so this
/// state—not `scene_budget_bytes`—tracks whether that command actually reached the ledger and
/// completed the coordinator-local eviction pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSceneBudgetRetry {
    pub requested_budget: usize,
    pub clear_before_apply: bool,
    pub observed_policy_revision: u64,
    pub observed_budget: usize,
    pub pressure: LedgerError,
    pub ledger_epoch: LedgerProgressEpoch,
    /// The first UI-side snapshot proved that the exact worker-reported blocker had already
    /// cleared. This closes the settle-between-event-and-consumption wakeup race without opening
    /// a frame-rate retry loop.
    pub ready_after_observed_progress: bool,
    pub in_flight: bool,
}

impl AlignmentSceneBudgetRetry {
    pub fn after_pressure_event(
        requested_budget: usize,
        clear_before_apply: bool,
        observed_policy_revision: u64,
        observed_budget: usize,
        pressure: LedgerError,
        snapshot: &impl AlignmentLedgerProgressView,
    ) -> Self {
        let snapshot = snapshot.alignment_ledger_progress();
        let policy_already_applied = !clear_before_apply && snapshot.budget == requested_budget;
        Self {
            requested_budget,
            clear_before_apply,
            observed_policy_revision,
            observed_budget,
            pressure,
            ledger_epoch: snapshot.progress_epoch(),
            ready_after_observed_progress: policy_already_applied
                || !alignment_ledger_pressure_blocker_remains(pressure, &snapshot),
            in_flight: false,
        }
    }

    pub fn poll(
        mut self,
        snapshot: &impl AlignmentLedgerProgressView,
    ) -> AlignmentSceneBudgetRetryPoll {
        let snapshot = snapshot.alignment_ledger_progress();
        if self.in_flight {
            return AlignmentSceneBudgetRetryPoll::Waiting(self);
        }
        if self.ready_after_observed_progress {
            return AlignmentSceneBudgetRetryPoll::Ready;
        }

        // Another coordinator sharing this ledger may already have installed the requested base
        // policy. `set_budget` is then a no-op even during a newer prepared GPU transition, but
        // this coordinator must still run its own cache-eviction pass.
        if !self.clear_before_apply && snapshot.budget == self.requested_budget {
            return AlignmentSceneBudgetRetryPoll::Ready;
        }
        if self.ledger_epoch == snapshot.progress_epoch() {
            return AlignmentSceneBudgetRetryPoll::Waiting(self);
        }
        if alignment_ledger_pressure_blocker_remains(self.pressure, &snapshot) {
            self.ledger_epoch = snapshot.progress_epoch();
            AlignmentSceneBudgetRetryPoll::Waiting(self)
        } else {
            AlignmentSceneBudgetRetryPoll::Ready
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentSceneBudgetRetryPoll {
    Waiting(AlignmentSceneBudgetRetry),
    Ready,
}

#[cfg(test)]
mod alignment_scene_pressure_retry_tests {
    use super::*;
    use rgv_alignment_scene::AllocationBundleSpec;

    fn request() -> (AlignmentSceneRequestToken, GenomicInterval) {
        (
            AlignmentSceneRequestToken::new(17),
            GenomicInterval::new(ContigId::new(2), 100, 200).unwrap(),
        )
    }

    fn retry(ledger: &AllocationLedger, pressure: LedgerError) -> AlignmentScenePressureRetry {
        let (request_token, target) = request();
        AlignmentScenePressureRetry {
            request_token,
            target,
            pressure,
            ledger_epoch: ledger.snapshot().unwrap().progress_epoch(),
            in_flight: false,
        }
    }

    fn retained(ledger: &AllocationLedger, bytes: usize) -> rgv_alignment_scene::AllocationLease {
        let mut reservation = ledger
            .reserve(
                rgv_alignment_scene::AllocationClass::CoreChunk,
                bytes,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap();
        reservation.register(bytes, 1).unwrap()
    }

    fn assert_waiting(poll: AlignmentScenePressureRetryPoll) -> AlignmentScenePressureRetry {
        match poll {
            AlignmentScenePressureRetryPoll::Waiting(retry) => retry,
            AlignmentScenePressureRetryPoll::Ready => panic!("expected pressure to keep waiting"),
        }
    }

    #[test]
    fn unchanged_exact_pressure_remains_waiting() {
        let ledger = AllocationLedger::new(100, 0);
        let _occupied = retained(&ledger, 80);
        let pressure = ledger
            .reserve(
                rgv_alignment_scene::AllocationClass::CoreChunk,
                40,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap_err();
        let retry = retry(&ledger, pressure);

        assert_eq!(
            assert_waiting(retry.poll(&ledger.snapshot().unwrap())),
            retry
        );
    }

    #[test]
    fn first_pressure_waits_without_queueing_when_post_relief_blocker_remains() {
        let ledger = AllocationLedger::new(1_024, 128);
        let before_bundle = ledger.snapshot().unwrap();
        let bundle = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                before_bundle.policy_revision,
                Vec::new(),
                ledger.empty_retirement_claim_set(),
            ))
            .unwrap();
        let blocked = ledger.snapshot().unwrap();
        let pressure = LedgerError::ReplacementTransitionActive {
            bundle_id: blocked.active_replacement_bundle.unwrap(),
        };
        let (request_token, target) = request();

        let waiting = assert_waiting(alignment_scene_pressure_after_relief(
            request_token,
            target,
            pressure,
            &blocked,
        ));
        assert!(!waiting.in_flight);
        assert_eq!(waiting.ledger_epoch, blocked.progress_epoch());
        assert_eq!(assert_waiting(waiting.poll(&blocked)), waiting);

        drop(bundle);
        assert_eq!(
            waiting.poll(&ledger.snapshot().unwrap()),
            AlignmentScenePressureRetryPoll::Ready
        );
    }

    #[test]
    fn unrelated_pin_and_allocation_churn_cannot_wake_the_same_active_bundle() {
        let ledger = AllocationLedger::new(1_024, 128);
        let retained_for_pin = retained(&ledger, 64);
        let unrelated_allocation = retained(&ledger, 32);
        let before_bundle = ledger.snapshot().unwrap();
        let bundle = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                before_bundle.policy_revision,
                Vec::new(),
                ledger.empty_retirement_claim_set(),
            ))
            .unwrap();
        let bundle_id = ledger
            .snapshot()
            .unwrap()
            .active_replacement_bundle
            .unwrap();
        let retry = retry(
            &ledger,
            LedgerError::ReplacementTransitionActive { bundle_id },
        );

        let pin = retained_for_pin.pin().unwrap();
        let after_pin = ledger.snapshot().unwrap();
        assert_ne!(retry.ledger_epoch, after_pin.progress_epoch());
        let after_pin_retry = assert_waiting(retry.poll(&after_pin));
        assert_eq!(after_pin_retry.ledger_epoch, after_pin.progress_epoch());

        drop(unrelated_allocation);
        let after_release = ledger.snapshot().unwrap();
        let after_release_retry = assert_waiting(after_pin_retry.poll(&after_release));
        assert_eq!(
            after_release_retry.ledger_epoch,
            after_release.progress_epoch()
        );

        drop(bundle);
        assert_eq!(
            after_release_retry.poll(&ledger.snapshot().unwrap()),
            AlignmentScenePressureRetryPoll::Ready
        );
        drop(pin);
    }

    #[test]
    fn retained_pressure_waits_through_insufficient_churn_then_wakes_when_it_fits() {
        let ledger = AllocationLedger::new(100, 0);
        let occupied = retained(&ledger, 70);
        let unrelated = retained(&ledger, 10);
        let pressure = ledger
            .reserve(
                rgv_alignment_scene::AllocationClass::CoreChunk,
                40,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap_err();
        let retry = retry(&ledger, pressure);

        drop(unrelated);
        let still_blocked = ledger.snapshot().unwrap();
        let waiting = assert_waiting(retry.poll(&still_blocked));
        assert_eq!(waiting.ledger_epoch, still_blocked.progress_epoch());

        drop(occupied);
        assert_eq!(
            waiting.poll(&ledger.snapshot().unwrap()),
            AlignmentScenePressureRetryPoll::Ready
        );
    }

    #[test]
    fn retained_policy_change_makes_one_retry_eligible() {
        let ledger = AllocationLedger::new(100, 0);
        let _occupied = retained(&ledger, 80);
        let pressure = ledger
            .reserve(
                rgv_alignment_scene::AllocationClass::CoreChunk,
                40,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap_err();
        let retry = retry(&ledger, pressure);

        ledger.set_budget(200).unwrap();
        assert_eq!(
            retry.poll(&ledger.snapshot().unwrap()),
            AlignmentScenePressureRetryPoll::Ready
        );
    }

    #[test]
    fn transient_pressure_uses_current_occupancy_and_captured_policy_lane() {
        let ledger = AllocationLedger::new_with_transient_budget(1_024, 0, 100);
        let mut occupied = ledger
            .reserve_transient(
                rgv_alignment_scene::TransientResourceClass::DecoderScratch,
                70,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap();
        let unrelated = ledger
            .reserve_transient(
                rgv_alignment_scene::TransientResourceClass::DecoderScratch,
                10,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap();
        let pressure = ledger
            .reserve_transient(
                rgv_alignment_scene::TransientResourceClass::DecoderScratch,
                40,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap_err();
        let retry = retry(&ledger, pressure);

        drop(unrelated);
        let still_blocked = ledger.snapshot().unwrap();
        let waiting = assert_waiting(retry.poll(&still_blocked));

        occupied.shrink_to(50).unwrap();
        assert_eq!(
            waiting.poll(&ledger.snapshot().unwrap()),
            AlignmentScenePressureRetryPoll::Ready
        );
    }
}

#[cfg(test)]
mod alignment_scene_budget_retry_tests {
    use super::*;
    use rgv_alignment_scene::AllocationBundleSpec;

    fn active_bundle(
        ledger: &AllocationLedger,
    ) -> (rgv_alignment_scene::AllocationBundleReservation, u64) {
        let snapshot = ledger.snapshot().unwrap();
        let bundle = ledger
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
        (bundle, bundle_id)
    }

    fn retry(
        ledger: &AllocationLedger,
        requested_budget: usize,
        clear_before_apply: bool,
        bundle_id: u64,
    ) -> AlignmentSceneBudgetRetry {
        let snapshot = ledger.snapshot().unwrap();
        AlignmentSceneBudgetRetry {
            requested_budget,
            clear_before_apply,
            observed_policy_revision: snapshot.policy_revision,
            observed_budget: snapshot.budget,
            pressure: LedgerError::ReplacementTransitionActive { bundle_id },
            ledger_epoch: snapshot.progress_epoch(),
            ready_after_observed_progress: false,
            in_flight: false,
        }
    }

    fn assert_waiting(poll: AlignmentSceneBudgetRetryPoll) -> AlignmentSceneBudgetRetry {
        match poll {
            AlignmentSceneBudgetRetryPoll::Waiting(retry) => retry,
            AlignmentSceneBudgetRetryPoll::Ready => panic!("expected budget retry to wait"),
        }
    }

    #[test]
    fn active_replacement_waits_without_polling_then_wakes_once_after_settlement() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (bundle, bundle_id) = active_bundle(&ledger);
        let retry = retry(&ledger, 512, false, bundle_id);
        assert_eq!(
            assert_waiting(retry.poll(&ledger.snapshot().unwrap())),
            retry
        );

        drop(bundle);
        assert_eq!(
            retry.poll(&ledger.snapshot().unwrap()),
            AlignmentSceneBudgetRetryPoll::Ready
        );
    }

    #[test]
    fn shared_policy_application_still_runs_this_coordinators_eviction_pass() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (old_bundle, old_bundle_id) = active_bundle(&ledger);
        let retry = retry(&ledger, 512, false, old_bundle_id);
        drop(old_bundle);
        ledger.set_budget(512).unwrap();
        let (_new_bundle, _) = active_bundle(&ledger);

        assert_eq!(
            retry.poll(&ledger.snapshot().unwrap()),
            AlignmentSceneBudgetRetryPoll::Ready,
            "a shared ledger policy success must not skip this source's local eviction pass"
        );
    }

    #[test]
    fn clear_transaction_cannot_skip_zero_pass_when_restore_budget_is_already_current() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (_bundle, bundle_id) = active_bundle(&ledger);
        let retry = retry(&ledger, 1_024, true, bundle_id);

        assert_eq!(
            assert_waiting(retry.poll(&ledger.snapshot().unwrap())),
            retry,
            "the final restored policy does not prove that the clear pass ran"
        );
    }

    #[test]
    fn queued_retry_stays_single_flight_even_after_unrelated_progress() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (bundle, bundle_id) = active_bundle(&ledger);
        let mut retry = retry(&ledger, 512, false, bundle_id);
        retry.in_flight = true;
        drop(bundle);

        assert_eq!(
            assert_waiting(retry.poll(&ledger.snapshot().unwrap())),
            retry
        );
    }

    #[test]
    fn settlement_between_worker_event_and_ui_consumption_is_immediately_retryable() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (bundle, bundle_id) = active_bundle(&ledger);
        let observed = ledger.snapshot().unwrap();
        drop(bundle);
        let consumed = AlignmentSceneBudgetRetry::after_pressure_event(
            512,
            false,
            observed.policy_revision,
            observed.budget,
            LedgerError::ReplacementTransitionActive { bundle_id },
            &ledger.snapshot().unwrap(),
        );

        assert!(consumed.ready_after_observed_progress);
        assert_eq!(
            consumed.poll(&ledger.snapshot().unwrap()),
            AlignmentSceneBudgetRetryPoll::Ready,
            "a settled blocker must not wait for a second ledger epoch change"
        );
    }

    #[test]
    fn current_retry_response_refreshes_blocker_epoch_and_clears_single_flight() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (old_bundle, old_bundle_id) = active_bundle(&ledger);
        let mut in_flight = retry(&ledger, 512, false, old_bundle_id);
        in_flight.in_flight = true;
        drop(old_bundle);
        let (_current_bundle, current_bundle_id) = active_bundle(&ledger);
        let current = ledger.snapshot().unwrap();
        let refreshed = AlignmentSceneBudgetRetry::after_pressure_event(
            512,
            false,
            current.policy_revision,
            current.budget,
            LedgerError::ReplacementTransitionActive {
                bundle_id: current_bundle_id,
            },
            &current,
        );

        assert_ne!(in_flight.pressure, refreshed.pressure);
        assert_eq!(refreshed.ledger_epoch, current.progress_epoch());
        assert!(!refreshed.in_flight);
        assert!(!refreshed.ready_after_observed_progress);
        assert_eq!(
            assert_waiting(refreshed.poll(&current)),
            refreshed,
            "the refreshed exact blocker must wait rather than reusing prior relief"
        );
    }
}

/// Exact authority for one cached non-destructive highlight selection.
///
/// Evidence revision alone is insufficient: the same retained evidence root can back several
/// successively wider prepared scenes. The evaluation target therefore participates directly in
/// identity. Reference-backed predicates also name the exact immutable FASTA slice which was
/// available during evaluation, so a later, wider slice retries a formerly incomplete result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentHighlightCacheKey {
    pub evidence_revision: rgv_alignment_scene::EvidenceRevision,
    pub target: GenomicInterval,
    pub filter: ContractFingerprint,
    pub reference: Option<(ReferenceRevision, GenomicInterval)>,
}

#[derive(Default)]
pub struct AlignmentPanelProjection {
    /// Coverage-pyramid request and presented authority of this panel; see
    /// `workspace::alignment_coverage`.
    pub coverage: crate::alignment::coverage::AlignmentCoverageProjection,
    pub clock: GenerationClock,
    pub unavailable: Option<String>,
    /// The viewport is at overview detail and is intentionally represented by
    /// the alignment index rather than a BAM/CRAM record query.
    pub index_only_overview: bool,
    /// Generation and start time of the in-flight exact viewport check. Its
    /// visible notice is delayed so fast queries do not flash a banner during
    /// ordinary panning.
    pub checking_started: Option<(u64, Instant)>,
    pub checking_notice_visible: bool,
    pub last_scene_resolution: Option<AlignmentSceneResolution>,
    /// Unified-scene lane identity and latest immutable worker publication. The worker camera is
    /// acquisition intent only; rendering captures this snapshot against the UI's current camera.
    pub scene_owner: Option<SourceOwnerId>,
    pub scene_contig: Option<ContigId>,
    pub scene_contract: Option<AlignmentSceneBuildContract>,
    pub scene_policy: Option<AlignmentScenePreparationPolicy>,
    /// Sole liveness authority for the latest unified-scene target. No query-local state can
    /// settle or publish this record.
    pub navigation: AlignmentNavigationTransaction,
    /// Last exact pressure resume admitted for this request at a specific shared-ledger epoch.
    /// It is scheduling state only and never authorizes scene publication.
    pub scene_pressure_retry: Option<AlignmentScenePressureRetry>,
    /// Last optional-preparation foreground state accepted by the source worker mailbox.
    pub scene_optional_work_paused: bool,
    pub scene_frame: Option<AlignmentCoordinatorFrame>,
    pub scene_frame_request_token: Option<AlignmentSceneRequestToken>,
    pub scene_telemetry: AlignmentSceneCoordinatorTelemetry,
    pub scene_interaction: AlignmentInteractionState,
    pub scene_highlight_key: Option<AlignmentHighlightCacheKey>,
    pub scene_highlighted: Arc<[AlignmentHandle]>,
    pub scene_highlight_status: Option<String>,
}

impl AlignmentPanelProjection {
    pub fn invalidate_scene_highlight(&mut self) {
        self.scene_highlight_key = None;
        self.scene_highlighted = Arc::from([]);
        self.scene_highlight_status = None;
    }

    pub fn settle_failed(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.navigation.transition(
            AlignmentNavigationPhase::Failed,
            Arc::<str>::from(message.clone()),
        );
        self.finish_checking();
        self.scene_pressure_retry = None;
        self.unavailable = Some(message);
        self.index_only_overview = false;
    }

    pub fn settle_unavailable(&mut self, message: String) {
        self.navigation.transition(
            AlignmentNavigationPhase::Unavailable,
            Arc::<str>::from(message.clone()),
        );
        self.finish_checking();
        self.scene_pressure_retry = None;
        self.unavailable = Some(message);
        self.index_only_overview = false;
    }

    /// Starts a new immutable-scene generation. Previously committed scene pixels remain valid
    /// until a complete replacement epoch is ready.
    pub fn advance_scene_generation(&self) -> ViewportGeneration {
        self.clock.advance()
    }

    /// Whether the projection belongs to the track clock's current generation.
    /// `pending_query` covers work waiting to be dispatched; the generation
    /// comparison also covers a request that has been queued and is still
    /// running. A previous query must not be reprojected or exposed through
    /// interaction while either condition is true.
    pub fn scene_is_current(&self) -> bool {
        self.navigation
            .is_current_settled(self.clock.current().get())
    }

    pub fn scene_is_checking(&self) -> bool {
        !self.scene_is_current()
    }

    pub fn begin_checking(&mut self, now: Instant) {
        let generation = self.clock.current().get();
        if self.checking_started.map(|(seen, _)| seen) != Some(generation) {
            self.checking_started = Some((generation, now));
            self.checking_notice_visible = false;
        }
    }

    pub fn reveal_checking_notice_if_due(&mut self, now: Instant) -> bool {
        if self.checking_notice_visible || !self.scene_is_checking() {
            return false;
        }
        let generation = self.clock.current().get();
        let Some((started_generation, started)) = self.checking_started else {
            return false;
        };
        if started_generation != generation
            || now.saturating_duration_since(started) < READ_CHECKING_NOTICE_DELAY
        {
            return false;
        }
        self.checking_notice_visible = true;
        true
    }

    pub fn finish_checking(&mut self) {
        self.checking_started = None;
        self.checking_notice_visible = false;
    }

    pub fn settle_index_only_overview(&mut self, resolution: AlignmentSceneResolution) {
        debug_assert!(!resolution.requires_alignment_records());
        self.navigation.transition(
            AlignmentNavigationPhase::IndexOverview,
            "viewport uses exact index-only alignment overview",
        );
        self.finish_checking();
        self.unavailable = None;
        self.index_only_overview = true;
        self.last_scene_resolution = Some(resolution);
    }
}

pub const READ_CHECKING_NOTICE_DELAY: Duration = Duration::from_millis(150);
const RESIZE_COVERAGE_BIN_STEP: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSceneResolution {
    pub detail: DetailLevel,
    pub coverage_bins: usize,
    pub collect_alleles: bool,
}

impl AlignmentSceneResolution {
    pub fn for_viewport(viewport: Viewport) -> Self {
        let visible_bases = usize::try_from(viewport.interval().len()).unwrap_or(usize::MAX);
        let (coverage_bins, collect_alleles) = alignment_coverage_query_plan(
            visible_bases,
            viewport.pixel_width(),
            viewport.bases_per_pixel(),
        );
        Self {
            detail: viewport.detail_level(),
            coverage_bins,
            collect_alleles,
        }
    }

    pub fn requires_refresh_from(self, previous: Self) -> bool {
        self.detail != previous.detail
            || self.collect_alleles != previous.collect_alleles
            || self.coverage_bucket() != previous.coverage_bucket()
    }

    pub const fn requires_alignment_records(self) -> bool {
        !matches!(self.detail, DetailLevel::Overview)
    }

    const fn coverage_bucket(self) -> usize {
        self.coverage_bins / RESIZE_COVERAGE_BIN_STEP
    }
}

pub struct AlignmentTrackController {
    pub config: SessionAlignmentTrack,
    /// New source-scoped immutable-scene authority.
    pub scene_engine: Option<AlignmentSceneEngine<PositionedAlignmentBackend>>,
    /// Multi-resolution coverage worker of the same source (ARCH-1). Independent of the scene
    /// engine: its own source handle and thread, decoding below foreground reads.
    pub coverage_engine: Option<rgv_engine::AlignmentCoverageEngine>,
    /// Last engine publication counter the UI observed; a change redraws the coverage lane.
    pub coverage_publication_sequence: u64,
    pub scene_metadata: Option<AlignmentSceneSourceMetadata>,
    /// Last Complete-read admission policy submitted to the source actor. This avoids flooding
    /// the bounded control mailbox while still making the user setting live and retryable.
    pub scene_complete_read_limit: usize,
    pub lifecycle: AlignmentTrackLifecycle,
    pub dictionary: Option<ReferenceDictionary>,
    pub index_overview: Option<Arc<rgv_hts::AlignmentIndexOverview>>,
    pub filter_editor: FilterEditor,
    pub active_filters: ActiveFilters,
    pub projections: HashMap<PanelId, AlignmentPanelProjection>,
    /// Last shared scene-ledger limit submitted to the engine actor. Keeping the applied value
    /// here prevents a frame-by-frame update from flooding
    /// the bounded actor queue while its statistics event is in flight.
    pub scene_budget_bytes: NonZeroUsize,
    /// Typed, progress-gated authority proving that the latest queued budget edit still needs a
    /// coordinator-local retry. This is deliberately separate from `scene_budget_bytes`, which
    /// records submission and cannot prove worker completion.
    pub scene_budget_retry: Option<AlignmentSceneBudgetRetry>,
}

pub struct AlignmentSettingsDraft {
    pub filters: SessionFilters,
    pub filter_editor: FilterEditor,
    pub maximum_alignments: usize,
    pub apply_filter: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSettingsOutcome {
    pub configuration_changed: bool,
    pub query_changed: bool,
}

impl AlignmentTrackController {
    pub fn mark_unavailable(&mut self, panel_id: PanelId, message: String) {
        let projection = self.projections.entry(panel_id).or_default();
        projection.settle_unavailable(message);
    }

    pub fn projection(&self, panel_id: PanelId) -> Option<&AlignmentPanelProjection> {
        self.projections.get(&panel_id)
    }

    pub fn projection_mut(&mut self, panel_id: PanelId) -> &mut AlignmentPanelProjection {
        self.projections.entry(panel_id).or_default()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSceneBudgetSnapshot {
    pub budget_bytes: usize,
    pub transition_reserve_bytes: usize,
    pub transient_budget_bytes: usize,
    pub charged_bytes: usize,
    pub reserved_bytes: usize,
    pub reserved_transition_bytes: usize,
    pub transient_bytes: usize,
    pub transient_high_water_bytes: usize,
    pub pinned_bytes: usize,
    pub pinned_overage_bytes: usize,
    pub active_replacement_bundle: Option<u64>,
    pub refreshes: u64,
}

impl AlignmentSceneBudgetSnapshot {
    pub fn from_progress(progress: LedgerProgress, refreshes: u64) -> Self {
        Self {
            budget_bytes: progress.budget,
            transition_reserve_bytes: progress.transition_reserve,
            transient_budget_bytes: progress.transient_budget,
            charged_bytes: progress.charged,
            reserved_bytes: progress.reserved,
            reserved_transition_bytes: progress.reserved_transition,
            transient_bytes: progress.transient,
            transient_high_water_bytes: progress.transient_high_water,
            pinned_bytes: progress.pinned,
            pinned_overage_bytes: progress.pinned_overage,
            active_replacement_bundle: progress.active_replacement_bundle,
            refreshes,
        }
    }
}

pub fn alignment_panel_projections(
    panel_ids: &[PanelId],
) -> HashMap<PanelId, AlignmentPanelProjection> {
    panel_ids
        .iter()
        .copied()
        .map(|id| (id, AlignmentPanelProjection::default()))
        .collect()
}

pub const MAXIMUM_COVERAGE_BINS: usize = 4_096;
pub const MAXIMUM_ALLELE_COVERAGE_BASES_PER_PIXEL: f64 = 2.0;

pub fn alignment_active_filter_count(filters: &SessionFilters) -> usize {
    filters
        .alignment_view_filter
        .all
        .len()
        .saturating_add(usize::from(filters.minimum_mapping_quality.is_some()))
        .saturating_add(usize::from(!filters.include_secondary))
        .saturating_add(usize::from(!filters.include_supplementary))
        .saturating_add(filters.auxiliary_predicates.len())
}

pub fn alignment_coverage_query_plan(
    visible_bases: usize,
    pixel_width: f64,
    bases_per_pixel: f64,
) -> (usize, bool) {
    let visible_bases = visible_bases.max(1);
    let collect_alleles = bases_per_pixel <= MAXIMUM_ALLELE_COVERAGE_BASES_PER_PIXEL
        && visible_bases <= MAXIMUM_COVERAGE_BINS;
    let bins = if collect_alleles {
        // Allele composition is meaningful only when each pileup bin maps to
        // one reference base. Keep this exact representation bounded by the
        // same hard cap used for ordinary screen-resolution coverage.
        visible_bases
    } else {
        (pixel_width.round() as usize)
            .clamp(64, MAXIMUM_COVERAGE_BINS)
            .min(visible_bases)
            .max(1)
    };
    (bins, collect_alleles)
}

/// Source-level options for the unified scene. Presentation and summary contracts remain typed
/// and independent; these options contain only source eligibility, resumable decode policy, and
/// the richest payload currently required by desktop alignment consumers.
pub fn alignment_scene_source_query_options(
    config: &SessionAlignmentTrack,
    _active_filters: &ActiveFilters,
) -> QueryOptions {
    let filters = &config.filters;
    let mut retained_auxiliary_tags = alignment_row_policy_auxiliary_tags(filters);
    retained_auxiliary_tags.push(*b"XS");
    if let AlignmentColorMode::Tag(tag) = filters.alignment_color_mode {
        retained_auxiliary_tags.push(tag);
    }
    retained_auxiliary_tags.sort_unstable();
    retained_auxiliary_tags.dedup();
    QueryOptions {
        detail: QueryDetail::Full,
        retained_auxiliary_tags,
        layout_requires_sequence: true,
        max_records: NonZeroUsize::new(config.effective_complete_read_limit())
            .expect("the complete read limit is non-zero"),
        collect_alleles: false,
        collect_modifications: false,
        overlapping_mate_policy: filters.overlapping_mate_policy,
        minimum_base_quality: filters.minimum_base_quality,
        minimum_junction_flank: filters.minimum_junction_flank,
        // Interactive eligibility is a derived immutable view over this retained superset. These
        // records must not disappear from canonical evidence when a chip is toggled or cleared.
        include_secondary: true,
        include_supplementary: true,
        include_reference: true,
        filter: AlignmentFilter::default(),
        ..QueryOptions::default()
    }
}

pub fn alignment_view_filter_contract(
    filters: &SessionFilters,
    active: &ActiveFilters,
    dictionary: &ReferenceDictionary,
) -> AlignmentViewFilterContract {
    let predicates = alignment_effective_view_predicates(filters, active)
        .into_iter()
        .map(|predicate| compile_alignment_view_predicate(&predicate, dictionary));
    AlignmentViewFilterContract::new(filters.alignment_view_filter.version, predicates)
}

/// Canonical user-visible predicate order, including the older controls which have not yet been
/// removed from the session schema. Counts and exports use this same order so provenance never
/// exposes an unlabeled prefix before the typed expression.
pub fn alignment_effective_view_predicates(
    filters: &SessionFilters,
    active: &ActiveFilters,
) -> Vec<AlignmentViewPredicate> {
    let mut predicates = Vec::new();
    if let Some(minimum) = active.alignment.minimum_mapping_quality {
        predicates.push(AlignmentViewPredicate::MappingQuality {
            minimum: Some(minimum),
            maximum: None,
        });
    }
    if !active.include_secondary {
        predicates.push(AlignmentViewPredicate::Flag {
            flag: rgv_core::AlignmentFlagPredicate::Secondary,
            expected: false,
        });
    }
    if !active.include_supplementary {
        predicates.push(AlignmentViewPredicate::Flag {
            flag: rgv_core::AlignmentFlagPredicate::Supplementary,
            expected: false,
        });
    }
    predicates.extend(active.alignment.auxiliary.iter().map(|predicate| {
        AlignmentViewPredicate::Auxiliary {
            tag: predicate.tag,
            comparison: match predicate.comparison {
                AuxComparison::Exists => AlignmentAuxComparison::Exists,
                AuxComparison::Equal => AlignmentAuxComparison::Equal,
                AuxComparison::NotEqual => AlignmentAuxComparison::NotEqual,
                AuxComparison::Greater => AlignmentAuxComparison::Greater,
                AuxComparison::GreaterOrEqual => AlignmentAuxComparison::GreaterOrEqual,
                AuxComparison::Less => AlignmentAuxComparison::Less,
                AuxComparison::LessOrEqual => AlignmentAuxComparison::LessOrEqual,
            },
            value: predicate.value.as_ref().map(|value| match value {
                FilterValue::Integer(value) => AlignmentAuxFilterValue::Integer(*value),
                FilterValue::Float(value) => AlignmentAuxFilterValue::FloatBits(value.to_bits()),
                FilterValue::String(value) => AlignmentAuxFilterValue::String(value.clone()),
            }),
        }
    }));
    predicates.extend(filters.alignment_view_filter.all.iter().cloned());
    predicates
}

pub fn alignment_highlight_filter_contract(
    filters: &SessionFilters,
    dictionary: &ReferenceDictionary,
) -> AlignmentViewFilterContract {
    let predicates = filters
        .alignment_highlight_filter
        .all
        .iter()
        .map(|predicate| compile_alignment_view_predicate(predicate, dictionary));
    AlignmentViewFilterContract::new(filters.alignment_highlight_filter.version, predicates)
}

fn compile_alignment_filter_scope(
    scope: &AlignmentPredicateScope,
    dictionary: &ReferenceDictionary,
) -> AlignmentFilterScope {
    match scope {
        AlignmentPredicateScope::Anywhere => AlignmentFilterScope::Anywhere,
        AlignmentPredicateScope::Position { contig, position } => dictionary
            .id_by_name(contig)
            .map_or(AlignmentFilterScope::UnresolvedContig, |contig| {
                AlignmentFilterScope::Position {
                    contig,
                    position: *position,
                }
            }),
        AlignmentPredicateScope::Interval { contig, start, end } => dictionary
            .id_by_name(contig)
            .and_then(|contig| GenomicInterval::new(contig, *start, *end).ok())
            .map_or(AlignmentFilterScope::UnresolvedContig, |interval| {
                AlignmentFilterScope::Interval { interval }
            }),
    }
}

fn compile_alignment_view_predicate(
    predicate: &AlignmentViewPredicate,
    dictionary: &ReferenceDictionary,
) -> AlignmentFilterPredicate {
    match predicate {
        AlignmentViewPredicate::MappingQuality { minimum, maximum } => {
            AlignmentFilterPredicate::MappingQuality {
                minimum: *minimum,
                maximum: *maximum,
            }
        }
        AlignmentViewPredicate::Flag { flag, expected } => AlignmentFilterPredicate::Flag {
            flag: *flag,
            expected: *expected,
        },
        AlignmentViewPredicate::TemplateLength { minimum, maximum } => {
            AlignmentFilterPredicate::TemplateLength {
                minimum: *minimum,
                maximum: *maximum,
            }
        }
        AlignmentViewPredicate::MateContig { contig } => AlignmentFilterPredicate::MateContig {
            contig: dictionary.id_by_name(contig),
        },
        AlignmentViewPredicate::Auxiliary {
            tag,
            comparison,
            value,
        } => AlignmentFilterPredicate::Auxiliary {
            tag: *tag,
            comparison: *comparison,
            value: value.clone(),
        },
        AlignmentViewPredicate::CigarEvent {
            event,
            scope,
            minimum_length,
            maximum_length,
        } => AlignmentFilterPredicate::CigarEvent {
            event: *event,
            scope: compile_alignment_filter_scope(scope, dictionary),
            minimum_length: *minimum_length,
            maximum_length: *maximum_length,
        },
        AlignmentViewPredicate::AlignedLength { minimum, maximum } => {
            AlignmentFilterPredicate::AlignedLength {
                minimum: *minimum,
                maximum: *maximum,
            }
        }
        AlignmentViewPredicate::Substitution {
            scope,
            observed_base,
            minimum_count,
            maximum_count,
        } => AlignmentFilterPredicate::Substitution {
            scope: compile_alignment_filter_scope(scope, dictionary),
            observed_base: *observed_base,
            minimum_count: *minimum_count,
            maximum_count: *maximum_count,
        },
    }
}

/// Converts the persisted read-sampling setting into the scene layout contract.
pub fn scene_read_sampling(sampling: AlignmentReadSampling) -> ReadSampling {
    match sampling.normalized() {
        AlignmentReadSampling::Complete => ReadSampling::Complete,
        AlignmentReadSampling::Downsampled {
            window_bases,
            maximum_reads,
        } => ReadSampling::Downsampled {
            window_bases: std::num::NonZeroU32::new(window_bases)
                .expect("normalized sampling window is non-zero"),
            maximum_reads: std::num::NonZeroU32::new(maximum_reads)
                .expect("normalized sampling limit is non-zero"),
        },
    }
}

pub fn alignment_scene_build_contract(
    config: &SessionAlignmentTrack,
    active_filters: &ActiveFilters,
    metadata: &AlignmentSceneSourceMetadata,
    contig: ContigId,
    contig_length: u64,
    viewport: Viewport,
    promoted_template: Option<u64>,
    reference_revision: Option<ReferenceRevision>,
) -> AlignmentSceneBuildContract {
    let filters = &config.filters;
    let detail = viewport.detail_level();
    // A coverage allele becomes a substitution only relative to a versioned FASTA base. Keep
    // exact depth reference-independent, but do not publish an allele presentation contract that
    // could cause every observed base to be labelled as an alternate when no reference authority
    // is available.
    let allele_detail = if reference_revision.is_some()
        && viewport.bases_per_pixel() <= MAXIMUM_ALLELE_COVERAGE_BASES_PER_PIXEL
        && viewport.interval().len() <= MAXIMUM_COVERAGE_BINS as u64
    {
        if filters.quality_weighted_alleles {
            AlleleSummaryDetail::QualityWeighted
        } else {
            AlleleSummaryDetail::Counts
        }
    } else {
        AlleleSummaryDetail::None
    };
    let mut hidden_modification_codes = [None; MAXIMUM_HIDDEN_MODIFICATION_CODES];
    for (slot, code) in hidden_modification_codes.iter_mut().zip(
        filters
            .hidden_modification_codes
            .iter()
            .filter_map(|value| parse_modification_code(value)),
    ) {
        *slot = Some(code);
    }
    let geometry_lod = alignment_scene_geometry_lod(detail, viewport.bases_per_pixel());
    let requested_view_filter =
        alignment_view_filter_contract(filters, active_filters, &metadata.dictionary);
    let requested_highlight_filter =
        alignment_highlight_filter_contract(filters, &metadata.dictionary);
    // A portable session can contain a reference-backed predicate before its FASTA has opened.
    // Keep the previously complete/unfiltered authority usable and expose the missing capability
    // in the UI; never submit a filter which the coordinator could only leave pending forever.
    let view_filter = if requested_view_filter.requires_reference() && reference_revision.is_none()
    {
        AlignmentViewFilterContract::default()
    } else {
        requested_view_filter.clone()
    };
    let highlight_filter =
        if requested_highlight_filter.requires_reference() && reference_revision.is_none() {
            AlignmentViewFilterContract::default()
        } else {
            requested_highlight_filter.clone()
        };
    let scene_reference_revision = if allele_detail != AlleleSummaryDetail::None
        || requested_view_filter.requires_reference()
        || requested_highlight_filter.requires_reference()
    {
        reference_revision
    } else {
        alignment_scene_reference_revision(geometry_lod, reference_revision)
    };
    AlignmentSceneBuildContract {
        source_revision: metadata.source_revision,
        record_contract: metadata.record_contract,
        contig,
        contig_length,
        complete_read_limit: NonZeroUsize::new(config.effective_complete_read_limit())
            .expect("the complete read limit is non-zero"),
        // Complete record inspection/export is a product consumer. Retain the full payload once
        // in canonical evidence rather than creating a second query authority on demand.
        payload: PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        ),
        view_filter,
        highlight_filter,
        summaries: SummaryContract {
            overlapping_mate_policy: filters.overlapping_mate_policy,
            minimum_base_quality: filters.minimum_base_quality,
            minimum_junction_flank: filters.minimum_junction_flank,
            allele_detail,
            collect_modifications: filters.show_modification_frequency,
        },
        layout: LayoutContract {
            stacking: filters.read_stacking_mode,
            grouping: filters.alignment_group_mode,
            group_order: filters.alignment_group_order,
            sorting: filters.alignment_sort_mode,
            sort_order: filters.alignment_sort_order,
            sort_anchor: if matches!(
                filters.alignment_group_mode,
                AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
            ) {
                filters.alignment_sort_anchor
            } else {
                alignment_sort_position(
                    filters.alignment_sort_mode,
                    filters.alignment_sort_anchor,
                    viewport,
                )
            },
            view_as_pairs: filters.view_as_pairs,
            pair_length_bounds: TemplateLengthBounds {
                minimum: filters.minimum_template_length,
                maximum: filters.maximum_template_length,
            },
            promoted_template,
            sampling: scene_read_sampling(filters.read_sampling),
        },
        presentation: PresentationContract {
            geometry_lod,
            style: AlignmentStyleContract {
                color_mode: filters.alignment_color_mode,
                base_display: filters.read_base_display_mode,
                shade_bases_by_quality: filters.shade_read_bases_by_quality,
                minimum_indel_size: filters.minimum_indel_size,
                minimum_event_label_size: filters.minimum_event_label_size,
                read_labels: AlignmentReadLabelMode::Hidden,
                show_direction_terminals: true,
                show_modified_bases: filters.show_modified_bases,
                minimum_modification_probability: filters.minimum_modification_probability,
                modified_base_color_mode: filters.modified_base_color_mode,
                hidden_modification_codes,
            },
            // Base semantics name the currently open immutable FASTA authority. The actual slice
            // is installed asynchronously for the coordinator's exact requested horizon.
            reference: scene_reference_revision,
            annotation: None,
        },
        geometry_tile_bases: NonZeroU64::new(DESKTOP_ALIGNMENT_SCENE_TILE_BASES)
            .expect("desktop scene tile size is non-zero"),
        summary_tile_bases: NonZeroU64::new(DESKTOP_ALIGNMENT_SCENE_TILE_BASES)
            .expect("desktop summary tile size is non-zero"),
        row_block_rows: NonZeroU32::new(DESKTOP_ALIGNMENT_SCENE_ROW_BLOCK_ROWS)
            .expect("desktop row block size is non-zero"),
        normalization_mapped_records: metadata.normalization_mapped_records,
        reference: None,
        annotation: None,
    }
}

/// Chooses semantic read geometry independently from the tighter text-legibility LOD.
///
/// A single base is already a meaningful coloured mark once it occupies one screen pixel. Base
/// letters remain camera-projected and are independently suppressed until their cells are wide
/// and tall enough to read. Using `DetailLevel::Bases` as the sole boundary would incorrectly
/// hide the colour-only substitution tier between one and eight pixels per base.
pub const fn alignment_scene_geometry_lod(
    detail: DetailLevel,
    bases_per_pixel: f64,
) -> GeometryLod {
    if bases_per_pixel <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL {
        GeometryLod::Base
    } else {
        match detail {
            DetailLevel::Overview => GeometryLod::Overview,
            DetailLevel::Regional => GeometryLod::Span,
            DetailLevel::Reads => GeometryLod::Read,
            // A caller-provided `Bases` detail remains authoritative even if its floating-point
            // scale has crossed the nominal boundary by a rounding epsilon.
            DetailLevel::Bases => GeometryLod::Base,
        }
    }
}

/// Base geometry compares retained read sequence against an immutable FASTA authority. Coarser
/// geometry stays reference-independent so opening or replacing a FASTA cannot invalidate the
/// resident Span/Read stack.
pub const fn alignment_scene_reference_revision(
    geometry_lod: GeometryLod,
    reference_revision: Option<ReferenceRevision>,
) -> Option<ReferenceRevision> {
    if matches!(geometry_lod, GeometryLod::Base) {
        reference_revision
    } else {
        None
    }
}

pub fn alignment_scene_preparation_policy(
    buffer_percent: u16,
    camera: impl Into<AlignmentSceneCamera>,
) -> AlignmentScenePreparationPolicy {
    let camera = camera.into();
    let side_viewports = f64::from(buffer_percent) / 100.0;
    let has_visible_rows = camera.has_visible_rows();
    let visible_rows = camera.visible_rows().len();
    let visible_rows = u32::try_from(visible_rows).unwrap_or(u32::MAX);
    let scaled_rows = |denominator: u64| {
        u64::from(visible_rows)
            .saturating_mul(u64::from(buffer_percent))
            .saturating_add(denominator.saturating_sub(1))
            .checked_div(denominator)
            .unwrap_or_default()
            .min(u64::from(u32::MAX)) as u32
    };
    AlignmentScenePreparationPolicy::new(
        side_viewports,
        side_viewports,
        side_viewports * 0.25,
        scaled_rows(100),
        if has_visible_rows {
            scaled_rows(400).max(1)
        } else {
            0
        },
    )
    .expect("desktop scene preparation policy is finite and non-negative")
    .with_full_vertical_residency(has_visible_rows)
}

pub fn alignment_scene_contract_equivalent(
    left: &AlignmentSceneBuildContract,
    right: &AlignmentSceneBuildContract,
) -> bool {
    left.source_revision == right.source_revision
        && left.record_contract == right.record_contract
        && left.contig == right.contig
        && left.contig_length == right.contig_length
        && left.complete_read_limit == right.complete_read_limit
        && left.payload == right.payload
        && left.view_filter == right.view_filter
        && left.highlight_filter == right.highlight_filter
        && left.summaries == right.summaries
        && left.layout == right.layout
        && left.presentation == right.presentation
        && left.geometry_tile_bases == right.geometry_tile_bases
        && left.summary_tile_bases == right.summary_tile_bases
        && left.row_block_rows == right.row_block_rows
        && left.normalization_mapped_records == right.normalization_mapped_records
        && left
            .reference
            .as_ref()
            .map(|authority| authority.revision())
            == right
                .reference
                .as_ref()
                .map(|authority| authority.revision())
        && left
            .annotation
            .as_ref()
            .map(|annotation| annotation.revision())
            == right
                .annotation
                .as_ref()
                .map(|annotation| annotation.revision())
}

fn alignment_row_policy_auxiliary_tags(filters: &SessionFilters) -> Vec<[u8; 2]> {
    let mut tags = Vec::new();
    match filters.alignment_group_mode {
        AlignmentGroupMode::ReadGroup => tags.push(*b"RG"),
        AlignmentGroupMode::Haplotype => tags.push(*b"HP"),
        AlignmentGroupMode::Chimeric => tags.push(*b"SA"),
        AlignmentGroupMode::Tag(tag) => tags.push(tag),
        _ => {}
    }
    match filters.alignment_sort_mode {
        AlignmentSortMode::ReadGroup => tags.push(*b"RG"),
        AlignmentSortMode::Haplotype => tags.push(*b"HP"),
        AlignmentSortMode::Tag(tag) => tags.push(tag),
        _ => {}
    }
    tags.sort_unstable();
    tags.dedup();
    tags
}

pub const fn alignment_color_mode_needs_reference(color_mode: AlignmentColorMode) -> bool {
    matches!(
        color_mode,
        AlignmentColorMode::Bisulfite(_) | AlignmentColorMode::Ultima(_)
    )
}

pub fn commit_alignment_settings_draft(
    config: &mut SessionAlignmentTrack,
    filter_editor: &mut FilterEditor,
    active_filters: &mut ActiveFilters,
    mut update: AlignmentSettingsDraft,
    workspace_limit: usize,
) -> AlignmentSettingsOutcome {
    let old_filters = config.filters.clone();
    let old_limit = config.maximum_alignments;
    let mut query_changed = old_filters.minimum_base_quality != update.filters.minimum_base_quality
        || old_filters.overlapping_mate_policy != update.filters.overlapping_mate_policy
        || old_filters.show_modification_frequency != update.filters.show_modification_frequency
        || old_filters.minimum_junction_flank != update.filters.minimum_junction_flank
        || alignment_color_mode_needs_reference(old_filters.alignment_color_mode)
            != alignment_color_mode_needs_reference(update.filters.alignment_color_mode);
    if update.apply_filter {
        let expressions = update
            .filter_editor
            .expression
            .split(';')
            .map(str::trim)
            .filter(|expression| !expression.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let parsed = expressions
            .iter()
            .map(|expression| expression.parse::<AuxPredicate>())
            .collect::<Result<Vec<_>, _>>();
        match parsed {
            Ok(auxiliary) => {
                let next_active = ActiveFilters {
                    alignment: AlignmentFilter {
                        minimum_mapping_quality: (update.filter_editor.minimum_mapq > 0)
                            .then_some(update.filter_editor.minimum_mapq),
                        auxiliary,
                        ..AlignmentFilter::default()
                    },
                    include_secondary: update.filter_editor.include_secondary,
                    include_supplementary: update.filter_editor.include_supplementary,
                };
                query_changed |= active_filters.alignment.fingerprint()
                    != next_active.alignment.fingerprint()
                    || active_filters.include_secondary != next_active.include_secondary
                    || active_filters.include_supplementary != next_active.include_supplementary;
                update.filters.minimum_mapping_quality =
                    next_active.alignment.minimum_mapping_quality;
                update.filters.include_secondary = next_active.include_secondary;
                update.filters.include_supplementary = next_active.include_supplementary;
                update.filters.auxiliary_predicates = expressions;
                *active_filters = next_active;
                update.filter_editor.error = None;
            }
            Err(error) => update.filter_editor.error = Some(error.to_string()),
        }
    }
    update.filters.read_sampling = update.filters.read_sampling.normalized();
    let filters_changed = old_filters != update.filters;
    let limit = update.maximum_alignments.min(workspace_limit).max(1);
    query_changed |= old_limit != limit
        || old_filters.read_sampling.is_downsampled()
            != update.filters.read_sampling.is_downsampled();
    *filter_editor = update.filter_editor;
    config.filters = update.filters;
    config.maximum_alignments = limit;
    AlignmentSettingsOutcome {
        configuration_changed: filters_changed || old_limit != limit,
        query_changed,
    }
}

pub fn copy_compatible_alignment_appearance(target: &mut SessionFilters, source: &SessionFilters) {
    target.alignment_color_mode = source.alignment_color_mode;
    target.alignment_group_mode = source.alignment_group_mode;
    target.alignment_group_order = source.alignment_group_order;
    target.alignment_sort_mode = source.alignment_sort_mode;
    target.alignment_sort_order = source.alignment_sort_order;
    target.alignment_sort_anchor = source.alignment_sort_anchor;
    target.read_stacking_mode = source.read_stacking_mode;
    target.read_sampling = source.read_sampling;
    target.view_as_pairs = source.view_as_pairs;
    target.minimum_template_length = source.minimum_template_length;
    target.maximum_template_length = source.maximum_template_length;
    target.show_reference = source.show_reference;
    target.show_coverage = source.show_coverage;
    target.show_junctions = source.show_junctions;
    target.minimum_junction_depth = source.minimum_junction_depth;
    target.junction_display_mode = source.junction_display_mode;
    target.sashimi_scale_mode = source.sashimi_scale_mode;
    target.sashimi_fixed_scale_maximum = source.sashimi_fixed_scale_maximum;
    target.sashimi_count_mode = source.sashimi_count_mode;
    target.show_junction_labels = source.show_junction_labels;
    target.show_reads = source.show_reads;
    target.coverage_scale_maximum = source.coverage_scale_maximum;
    target.coverage_scale_transform = source.coverage_scale_transform;
    target.show_coverage_center_line = source.show_coverage_center_line;
    target.alignment_density = source.alignment_density;
    target.inspector_activation = source.inspector_activation;
    target.read_base_display_mode = source.read_base_display_mode;
    target.shade_read_bases_by_quality = source.shade_read_bases_by_quality;
    target.minimum_indel_size = source.minimum_indel_size;
    target.minimum_event_label_size = source.minimum_event_label_size;
    target.show_modified_bases = source.show_modified_bases;
    target.show_modification_frequency = source.show_modification_frequency;
    target.minimum_modification_probability = source.minimum_modification_probability;
    target.modified_base_color_mode = source.modified_base_color_mode;
    target
        .hidden_modification_codes
        .clone_from(&source.hidden_modification_codes);
}

/// Whether a local path or URL names a CRAM, ignoring any URL query string or fragment.
pub fn alignment_source_is_cram(location: &str) -> bool {
    let path = if location.starts_with("http://") || location.starts_with("https://") {
        location.split(['?', '#']).next().unwrap_or(location)
    } else {
        location
    };
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cram"))
}

/// Connects each track's scene engine to `reference_engine`, so reference slices reach the
/// scene worker straight from the reference thread (ARCH-9) instead of only through the UI
/// relay. A reference engine drops its sinks when it is retired and a new scene engine has
/// none, so this runs once for every new (reference engine, scene engine) pair: for all tracks
/// when a reference engine is spawned, and for the new tracks when tracks are installed.
pub fn connect_reference_slice_sinks<'a>(
    reference_engine: Option<&rgv_engine::ReferenceEngine>,
    tracks: impl IntoIterator<Item = &'a AlignmentTrackController>,
) {
    let Some(reference_engine) = reference_engine else {
        return;
    };
    for scene_engine in tracks
        .into_iter()
        .filter_map(|track| track.scene_engine.as_ref())
    {
        reference_engine.connect_slice_sink(scene_engine.reference_slice_sink());
    }
}

/// Where and how one alignment source is opened: the path (or URL) and the open options shared
/// by its scene engine and its coverage engine.
pub fn alignment_source_open_request(
    config: &SessionAlignmentTrack,
    reference_path: Option<PathBuf>,
    remote_reference_index_url: Option<String>,
) -> (PathBuf, rgv_hts::AlignmentOpenOptions) {
    let source_location = match &config.source {
        TrackSource::Local { path } => path.clone(),
        TrackSource::Remote { url } => url.clone(),
    };
    // BAM decoding is reference-independent: FASTA authority is validated against the opened
    // dictionary and installed into semantic scenes separately, so no reference is handed to
    // the BAM decoder. CRAM is reference-compressed and cannot be decoded without one.
    let decode_reference = alignment_source_is_cram(&source_location)
        .then_some(reference_path)
        .flatten();
    (
        PathBuf::from(source_location),
        alignment_open_options(
            decode_reference,
            config.remote_index_url.clone(),
            remote_reference_index_url,
        ),
    )
}

pub fn spawn_alignment_track_controller(
    config: SessionAlignmentTrack,
    cache_bytes: NonZeroUsize,
    scene_ledger: AllocationLedger,
    reference_path: Option<PathBuf>,
    remote_reference_index_url: Option<String>,
    panel_ids: &[PanelId],
) -> Result<AlignmentTrackController> {
    let (filter_editor, active_filters) = filter_state_from_session(&config.filters)?;
    let (source_path, open_options) =
        alignment_source_open_request(&config, reference_path, remote_reference_index_url);
    let query_options = alignment_scene_source_query_options(&config, &active_filters);
    let (scene_engine, lifecycle) = (
        Some(AlignmentSceneEngine::spawn_local_bam(
            LocalAlignmentSceneEngineConfig {
                path: source_path,
                open_options,
                query_options,
                ledger: scene_ledger,
                mailbox_control_capacity: NonZeroUsize::new(32)
                    .expect("scene mailbox capacity is non-zero"),
                source_control_capacity: NonZeroUsize::new(8)
                    .expect("scene source control capacity is non-zero"),
                maximum_source_control_burst: NonZeroUsize::new(4)
                    .expect("scene source control burst is non-zero"),
            },
        )),
        AlignmentTrackLifecycle::Opening,
    );
    let scene_complete_read_limit = config.effective_complete_read_limit();
    Ok(AlignmentTrackController {
        config,
        scene_engine,
        // Started when the scene engine reports the source open, from its dictionary and
        // stamp (`Viewer::start_alignment_coverage_engine`): coverage is only asked of a
        // ready track, and a known source lets a sidecar-served request open no file.
        coverage_engine: None,
        coverage_publication_sequence: 0,
        scene_metadata: None,
        scene_complete_read_limit,
        lifecycle,
        dictionary: None,
        index_overview: None,
        filter_editor,
        active_filters,
        projections: alignment_panel_projections(panel_ids),
        scene_budget_bytes: cache_bytes,
        scene_budget_retry: None,
    })
}

pub fn initial_filters(session: Option<&SessionDocument>) -> Result<(FilterEditor, ActiveFilters)> {
    let filters = session
        .and_then(primary_alignment_track)
        .map_or_else(SessionFilters::default, |track| track.filters.clone());
    filter_state_from_session(&filters)
}

pub fn filter_state_from_session(
    filters: &SessionFilters,
) -> Result<(FilterEditor, ActiveFilters)> {
    let active = active_filters_from_session(filters)?;
    let expression = filters.auxiliary_predicates.join(";");
    Ok((
        FilterEditor {
            minimum_mapq: filters.minimum_mapping_quality.unwrap_or_default(),
            include_secondary: filters.include_secondary,
            include_supplementary: filters.include_supplementary,
            expression,
            error: None,
        },
        active,
    ))
}

pub fn active_filters_from_session(filters: &SessionFilters) -> Result<ActiveFilters> {
    let auxiliary = filters
        .auxiliary_predicates
        .iter()
        .map(|expression| expression.parse::<AuxPredicate>())
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ActiveFilters {
        alignment: AlignmentFilter {
            minimum_mapping_quality: filters.minimum_mapping_quality,
            auxiliary,
            ..AlignmentFilter::default()
        },
        include_secondary: filters.include_secondary,
        include_supplementary: filters.include_supplementary,
    })
}

/// The read position a base sort is anchored at: the fixed anchor when there is one, otherwise
/// the centre base of the view. `None` for every other sort mode.
pub fn alignment_sort_position(
    mode: AlignmentSortMode,
    fixed_anchor: Option<u64>,
    viewport: Viewport,
) -> Option<u64> {
    if mode != AlignmentSortMode::Base {
        return None;
    }
    let interval = viewport.interval();
    fixed_anchor.or_else(|| {
        Some(
            interval
                .start()
                .saturating_add(interval.len() / 2)
                .min(interval.end().saturating_sub(1)),
        )
    })
}

/// A base-modification code as the settings spell it: `CHEBI:<number>` or one letter.
pub fn parse_modification_code(value: &str) -> Option<ModificationCode> {
    if let Some(code) = value.strip_prefix("CHEBI:") {
        return code.parse().ok().map(ModificationCode::Chebi);
    }
    let bytes = value.as_bytes();
    (bytes.len() == 1 && bytes[0].is_ascii_alphabetic()).then(|| ModificationCode::Letter(bytes[0]))
}
