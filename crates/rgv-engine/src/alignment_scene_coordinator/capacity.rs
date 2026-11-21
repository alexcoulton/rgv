use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    time::Instant,
};

use rgv_alignment_scene::{
    AlignmentSceneSnapshot, AllocationId, AllocationLease, EvidenceStoreError, LedgerError,
};
use rgv_core::GenomicInterval;

use crate::{CanonicalAlignmentSourceBackend, SourceOwnerId};

use super::{
    AlignmentCoordinatorEviction, AlignmentCoordinatorPressure, AlignmentForegroundCapacityPlan,
    AlignmentSceneCoordinatorError, AlignmentSceneSourceCoordinator, elapsed_nanoseconds,
    presentation_owner,
};

pub(crate) const MAXIMUM_FOREGROUND_WORKING_SET_ISLANDS: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ForegroundWorkingSetSample {
    pub(crate) interval: GenomicInterval,
    pub(crate) bytes: usize,
}

impl<B> AlignmentSceneSourceCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    pub(crate) fn observe_prepared_foreground_working_sets(
        &mut self,
    ) -> Result<(), AlignmentSceneCoordinatorError<B::Error>> {
        let observations = self
            .lanes
            .iter()
            .filter_map(|(owner, lane)| {
                let desired = lane.desired.as_ref()?;
                let scene = self.presentation.prepared_for(presentation_owner(*owner))?;
                (self.last_measured_scene_by_owner.get(owner).copied() != Some(scene.revision()))
                    .then(|| (*owner, desired.source_horizon, Arc::clone(scene)))
            })
            .collect::<Vec<_>>();
        for (owner, interval, scene) in observations {
            let bytes =
                alignment_scene_derivative_bytes(&scene).map_err(EvidenceStoreError::from)?;
            self.last_measured_scene_by_owner
                .insert(owner, scene.revision());
            self.record_foreground_working_set(interval, bytes);
        }
        Ok(())
    }

    pub(crate) fn record_foreground_working_set(
        &mut self,
        interval: GenomicInterval,
        bytes: usize,
    ) {
        if bytes == 0 {
            return;
        }
        if let Some(index) = self
            .foreground_working_set_history
            .iter()
            .rposition(|sample| sample.interval.overlaps(interval))
        {
            let mut sample = self
                .foreground_working_set_history
                .remove(index)
                .expect("the located foreground sample remains present");
            sample.bytes = sample.bytes.max(bytes);
            sample.interval = GenomicInterval::new(
                interval.contig(),
                sample.interval.start().min(interval.start()),
                sample.interval.end().max(interval.end()),
            )
            .expect("the union of two overlapping non-empty horizons remains non-empty");
            self.foreground_working_set_history.push_back(sample);
        } else {
            self.foreground_working_set_history
                .push_back(ForegroundWorkingSetSample { interval, bytes });
            if self.foreground_working_set_history.len() > MAXIMUM_FOREGROUND_WORKING_SET_ISLANDS {
                self.foreground_working_set_history.pop_front();
            }
        }
    }

    /// Calculates the stable aggregate low-water mark for one latest foreground demand. A
    /// measured matching island wins; the most recently measured working set is only a first-visit
    /// fallback. A failed allocation expands the island envelope so the next reclaim reserves the
    /// whole known demand instead of exactly one page.
    pub fn foreground_capacity_plan(
        &mut self,
        owner: SourceOwnerId,
        refused_foreground_bytes: usize,
    ) -> Result<AlignmentForegroundCapacityPlan, AlignmentSceneCoordinatorError<B::Error>> {
        self.observe_prepared_foreground_working_sets()?;
        let interval = self
            .lanes
            .get(&owner)
            .and_then(|lane| lane.desired.as_ref())
            .map(|desired| desired.source_horizon)
            .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        if refused_foreground_bytes > 0 {
            self.record_foreground_working_set(interval, refused_foreground_bytes);
        }
        let foreground_reserve_bytes =
            foreground_working_set_reserve(&self.foreground_working_set_history, interval)
                .max(refused_foreground_bytes);
        let progress = self
            .actor
            .store()
            .ledger()
            .progress()
            .map_err(EvidenceStoreError::from)?;
        let mut residency = self.presentation.cache_residency_inventory()?;
        residency.merge(self.actor.store().cache_residency_inventory()?);
        let low_water_target = alignment_foreground_low_water_target(
            progress.budget,
            progress.reserved,
            progress.pinned,
            foreground_reserve_bytes,
        );
        Ok(AlignmentForegroundCapacityPlan {
            budget: progress.budget,
            charged: progress.charged,
            pinned: progress.pinned,
            reserved: progress.reserved,
            foreground_reserve_bytes,
            low_water_target,
            return_cache_bytes: residency.recent_locus_return_cache_bytes,
            disposable_speculation_bytes: residency.disposable_speculation_bytes,
        })
    }

    /// Strictest low-water mark across every live lane, used when a desktop GPU replacement asks
    /// the source worker to release aggregate CPU ownership. This prevents an external exact-fit
    /// request from weakening the measured reserve already learned by the source coordinator.
    pub fn aggregate_foreground_low_water_target(
        &mut self,
    ) -> Result<Option<usize>, AlignmentSceneCoordinatorError<B::Error>> {
        self.observe_prepared_foreground_working_sets()?;
        let intervals = self
            .lanes
            .values()
            .filter_map(|lane| lane.desired.as_ref().map(|desired| desired.source_horizon))
            .collect::<Vec<_>>();
        if intervals.is_empty() {
            return Ok(None);
        }
        let reserve = intervals
            .iter()
            .map(|interval| {
                foreground_working_set_reserve(&self.foreground_working_set_history, *interval)
            })
            .max()
            .unwrap_or(0);
        let progress = self
            .actor
            .store()
            .ledger()
            .progress()
            .map_err(EvidenceStoreError::from)?;
        Ok(Some(alignment_foreground_low_water_target(
            progress.budget,
            progress.reserved,
            progress.pinned,
            reserve,
        )))
    }

    pub fn reduce_budget(
        &mut self,
        budget: usize,
    ) -> Result<AlignmentCoordinatorPressure, AlignmentSceneCoordinatorError<B::Error>> {
        for lane in self.lanes.values_mut() {
            // A budget change is an explicit new admission decision. A prior background refusal
            // must not permanently suppress work after the user grants more room.
            lane.vertical_residency_budget_limited = false;
            lane.vertical_residency_failed = false;
        }
        let speculation_was_pending = self.actor.schedule().desired_speculation().is_some()
            || self
                .actor
                .schedule()
                .active()
                .is_some_and(|work| work.kind() == crate::SourceIntentKind::Speculative);
        let aborted = self.actor.abort_speculation();
        if let Some(work) = &aborted
            && let Some(lane) = self.lanes.get_mut(&work.intent().owner())
        {
            lane.telemetry.resource_pressure_speculation_aborts = lane
                .telemetry
                .resource_pressure_speculation_aborts
                .saturating_add(1);
        }
        let presentation = self.presentation.reduce_budget(budget)?;
        if presentation.evicted_read_pages > 0 {
            self.reconcile_vertical_residency_after_reclaim();
        }
        let evidence = self.actor.store_mut().reduce_budget(budget)?;
        let progress = self
            .actor
            .store()
            .ledger()
            .progress()
            .map_err(EvidenceStoreError::from)?;
        Ok(AlignmentCoordinatorPressure {
            budget,
            speculation_was_pending,
            active_speculation_aborted: aborted.is_some(),
            presentation,
            evidence,
            charged: progress.charged,
            pinned: progress.pinned,
            pinned_overage: progress.pinned_overage,
            remaining_overage: progress.charged.saturating_sub(budget),
        })
    }

    /// Releases inactive source and semantic-page ownership toward an aggregate headroom goal
    /// without changing policy. This is deliberately deterministic (presentation pages first,
    /// then source/summary/layout caches), aborts optional read-ahead, and preserves every
    /// published pin.
    pub fn evict_inactive_until(
        &mut self,
        target_charged: usize,
    ) -> Result<AlignmentCoordinatorEviction, AlignmentSceneCoordinatorError<B::Error>> {
        self.evict_inactive_batch(target_charged, usize::MAX)
    }

    /// Performs one bounded cache-maintenance quantum. Foreground work remains on the same owner
    /// thread, but the engine can now inspect newer camera intent between quanta and cancel or
    /// retarget stale reclaim work.
    pub fn evict_inactive_batch(
        &mut self,
        target_charged: usize,
        maximum_entries: usize,
    ) -> Result<AlignmentCoordinatorEviction, AlignmentSceneCoordinatorError<B::Error>> {
        let started = Instant::now();
        let before = self
            .actor
            .store()
            .ledger()
            .progress()
            .map_err(EvidenceStoreError::from)?;
        let speculation_was_pending = self.actor.schedule().desired_speculation().is_some()
            || self
                .actor
                .schedule()
                .active()
                .is_some_and(|work| work.kind() == crate::SourceIntentKind::Speculative);
        let aborted = self.actor.abort_speculation();
        if let Some(work) = &aborted
            && let Some(lane) = self.lanes.get_mut(&work.intent().owner())
        {
            lane.telemetry.resource_pressure_speculation_aborts = lane
                .telemetry
                .resource_pressure_speculation_aborts
                .saturating_add(1);
        }
        let mut presentation = self
            .presentation
            .evict_inactive_batch(target_charged, maximum_entries)?;
        if presentation.evicted_read_pages > 0 {
            self.reconcile_vertical_residency_after_reclaim();
        }
        let presentation_entries = presentation.evicted_pages();
        let remaining_entries = maximum_entries.saturating_sub(presentation_entries);
        let mut evidence = self
            .actor
            .store_mut()
            .evict_inactive_batch(target_charged, remaining_entries)?;
        let progress = self
            .actor
            .store()
            .ledger()
            .progress()
            .map_err(EvidenceStoreError::from)?;
        let evidence_entries = evidence
            .evicted_acquisitions
            .saturating_add(evidence.evicted_chunks)
            .saturating_add(evidence.evicted_exact_summaries)
            .saturating_add(evidence.evicted_row_atlases);
        let relinquished_cache_entries = presentation_entries.saturating_add(evidence_entries);
        let reclaim_complete = progress.charged <= target_charged
            || (presentation.reclaim_complete && evidence.reclaim_complete);
        if reclaim_complete {
            presentation.residency = self.presentation.cache_residency_inventory()?;
            evidence.residency = self.actor.store().cache_residency_inventory()?;
        }
        Ok(AlignmentCoordinatorEviction {
            target_charged,
            speculation_was_pending,
            active_speculation_aborted: aborted.is_some(),
            presentation,
            evidence,
            charged_before: before.charged,
            charged: progress.charged,
            pinned: progress.pinned,
            released: before.charged.saturating_sub(progress.charged),
            relinquished_cache_entries,
            remaining: progress.charged.saturating_sub(target_charged),
            quantum_entries: relinquished_cache_entries,
            quanta: 1,
            elapsed_nanoseconds: elapsed_nanoseconds(started),
            reclaim_complete,
        })
    }

    pub(crate) fn reconcile_vertical_residency_after_reclaim(&mut self) {
        let presentation = &self.presentation;
        for (owner, lane) in &mut self.lanes {
            let removed = lane.reconcile_vertical_residency_cache(*owner, presentation);
            if removed > 0 {
                // Full-stack warming is disposable cache work. Do not immediately refill pages
                // just relinquished for foreground admission; a new preparation policy or source
                // horizon explicitly makes it eligible again.
                lane.vertical_residency_budget_limited = true;
            }
        }
    }
}

pub(crate) fn alignment_foreground_low_water_target(
    budget: usize,
    reserved: usize,
    pinned: usize,
    measured_foreground_reserve: usize,
) -> usize {
    budget
        .saturating_sub(measured_foreground_reserve)
        .saturating_sub(reserved)
        .max(pinned)
}

pub(crate) fn alignment_scene_derivative_bytes(
    scene: &AlignmentSceneSnapshot,
) -> Result<usize, LedgerError> {
    let mut allocations = HashSet::<AllocationId>::new();
    let mut bytes = 0_usize;
    observe_distinct_derivative_allocation(
        &mut allocations,
        &mut bytes,
        scene.metadata_allocation(),
    )?;
    observe_distinct_derivative_allocation(
        &mut allocations,
        &mut bytes,
        scene.reads().metadata_allocation(),
    )?;
    for page in scene.reads().pages() {
        observe_distinct_derivative_allocation(&mut allocations, &mut bytes, page.allocation())?;
    }
    for page in scene.summary_pages() {
        observe_distinct_derivative_allocation(&mut allocations, &mut bytes, page.allocation())?;
    }
    Ok(bytes)
}

pub(crate) fn foreground_working_set_reserve(
    history: &VecDeque<ForegroundWorkingSetSample>,
    interval: GenomicInterval,
) -> usize {
    let matching = history
        .iter()
        .filter(|sample| sample.interval.overlaps(interval))
        .map(|sample| sample.bytes)
        .max();
    // A locus which has not yet produced a measured scene inherits the bounded recent upper
    // envelope, not merely the last visited locus. Otherwise visiting one sparse locus after
    // ACTB would erase the only useful first-admission estimate and recreate exact-fit reclaim
    // when the next dense island was opened.
    matching.unwrap_or_else(|| history.iter().map(|sample| sample.bytes).max().unwrap_or(0))
}

pub(crate) fn observe_distinct_derivative_allocation(
    allocations: &mut HashSet<AllocationId>,
    bytes: &mut usize,
    allocation: &AllocationLease,
) -> Result<(), LedgerError> {
    if allocations.insert(allocation.id()) {
        *bytes = bytes.saturating_add(allocation.snapshot()?.charged_capacity);
    }
    Ok(())
}
