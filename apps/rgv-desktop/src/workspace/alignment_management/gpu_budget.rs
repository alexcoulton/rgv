use crate::workspace::alignment_management::gpu_publication::{
    AlignmentGpuPartialTopUp, AlignmentGpuPublicationRetryPoll, AlignmentGpuReclaimOutcome,
    alignment_capacity_reclaim_allows_retry, alignment_gpu_budget_pressure_blocker,
    alignment_gpu_partial_top_up_after_publication, alignment_gpu_partial_top_up_is_due,
    alignment_gpu_pressure_target_charged, alignment_gpu_publication_blocker_remains,
    alignment_gpu_publication_retry_gate, alignment_gpu_publication_retry_poll,
    alignment_partial_residency_status, alignment_source_pressure_gpu_budget,
};
use crate::workspace::alignments::AlignmentSceneBudgetSnapshot;
use crate::{
    AlignmentCapacityReclaimCycle, AlignmentCommittedLaneAuthority, AlignmentGpuPublicationBlocker,
    AlignmentGpuPublicationRetry, AlignmentGpuPublicationRetryGate, AlignmentGpuSpeculationPhase,
    AlignmentScenePublicationAttempt, Viewer,
};
use rgv_alignment_scene::{
    AlignmentPageKey, AllocationLedger, AllocationOwner, AllocationOwnerShares, LedgerError,
    LedgerProgressEpoch,
};
use rgv_core::TrackId;
use rgv_render::{AlignmentSummaryGpuPageKey, GpuBudgetPressure, GpuReclaimGoal, GpuReclaimReport};
use std::collections::HashSet;
use std::num::NonZeroUsize;

impl Viewer {
    /// Immediately releases every optional GPU pin when a foreground camera/navigation target
    /// begins. Renderer pages remain reusable and exact; only the speculative whole-stack
    /// residency promise is revoked, and the same authority may warm again after input settles.
    pub(crate) fn revoke_alignment_gpu_optional_residency_for_foreground(&mut self) {
        let (reads, summaries) = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .map_or((false, false), |bundle| {
                (bundle.reads.is_some(), bundle.summaries.is_some())
            });
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.reset(reads, summaries);
        if self.alignment_gpu_speculation_gate.authority.is_some() {
            self.alignment_gpu_speculation_gate.phase = AlignmentGpuSpeculationPhase::Eligible;
        }
    }

    pub(super) fn defer_alignment_gpu_publication(
        &mut self,
        attempt: &AlignmentScenePublicationAttempt,
        blocker: AlignmentGpuPublicationBlocker,
    ) {
        let snapshot = match self.alignment_gpu_ledger.progress() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.alignment_gpu_publication_retry = None;
                self.session.data_status =
                    format!("Could not inspect deferred alignment GPU capacity: {error}");
                return;
            }
        };
        let gate = alignment_gpu_publication_retry_gate(blocker, &snapshot);
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_publication_retry = Some(AlignmentGpuPublicationRetry {
            target_revision: attempt.target_revision,
            attempt_sequence: attempt.sequence,
            blocker,
            gate,
        });
        if matches!(
            gate,
            AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
        ) {
            self.window.request_redraw();
        }
    }

    /// Starts one aggregate foreground-reclaim cycle after the renderer-local owners have
    /// exhausted their legal eviction candidates. Each live source engine is asked exactly once
    /// to release inactive semantic/source ownership toward the same ledger target. The existing
    /// typed retry gate prevents redraws from reopening the GPU transaction while replies arrive.
    pub(super) fn defer_alignment_gpu_publication_for_cross_owner_reclaim(
        &mut self,
        attempt: &AlignmentScenePublicationAttempt,
        navigation_candidates: &[AlignmentCommittedLaneAuthority],
        pressure: GpuBudgetPressure,
    ) {
        let blocker = alignment_gpu_budget_pressure_blocker(pressure);
        self.wait_alignment_navigation_transactions_for_gpu(navigation_candidates);
        self.defer_alignment_gpu_publication(attempt, blocker);

        let snapshot = match self.alignment_gpu_ledger.progress() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.alignment_capacity_reclaim = None;
                self.session.data_status =
                    format!("Could not inspect aggregate alignment capacity: {error}");
                return;
            }
        };
        if !alignment_gpu_publication_blocker_remains(blocker, &snapshot) {
            // Renderer-local reclaim already crossed the exact threshold after the failure was
            // captured. The deferred gate is Ready and will issue the one permitted retry.
            return;
        }

        self.alignment_capacity_reclaim_clock = self
            .alignment_capacity_reclaim_clock
            .checked_add(1)
            .expect("alignment capacity reclaim request space was exhausted");
        let request_id = self.alignment_capacity_reclaim_clock;
        // A steady replacement may legitimately retain the old epoch until commit. Its exact
        // retirement credit therefore belongs in the physical occupancy ceiling; omitting it
        // would ask source/semantic owners to evict bytes already proved to disappear atomically.
        let target_charged = alignment_gpu_pressure_target_charged(pressure, &snapshot);
        let mut pending_tracks = HashSet::new();
        // Under the bounded policy GPU pages have a ledger of their own: no source engine holds
        // capacity which could admit this candidate, so none is asked and the cycle settles at
        // once instead of waiting on another layer.
        if !self.alignment_gpu_ledger_is_private() {
            for track in &self.session.alignment_tracks {
                let Some(engine) = &track.scene_engine else {
                    continue;
                };
                if engine
                    .try_evict_inactive_for_headroom(request_id, target_charged)
                    .is_ok()
                {
                    pending_tracks.insert(track.config.id);
                }
            }
        }
        self.alignment_capacity_reclaim = Some(AlignmentCapacityReclaimCycle {
            request_id,
            target_revision: attempt.target_revision,
            attempt_sequence: attempt.sequence,
            blocker,
            pressure,
            target_charged,
            pending_tracks,
            released_bytes: 0,
            relinquished_cache_entries: 0,
        });
        "Reclaiming inactive alignment cache for the current view…"
            .clone_into(&mut self.session.data_status);
        self.finish_alignment_capacity_reclaim_if_complete();
    }

    pub(crate) fn observe_alignment_capacity_reclaim(
        &mut self,
        track_id: TrackId,
        request_id: u64,
        released_bytes: usize,
        relinquished_cache_entries: usize,
    ) {
        let completed = {
            let Some(cycle) = self.alignment_capacity_reclaim.as_mut() else {
                return;
            };
            if cycle.request_id != request_id || !cycle.pending_tracks.remove(&track_id) {
                return;
            }
            cycle.released_bytes = cycle.released_bytes.saturating_add(released_bytes);
            cycle.relinquished_cache_entries = cycle
                .relinquished_cache_entries
                .saturating_add(relinquished_cache_entries);
            cycle.pending_tracks.is_empty()
        };
        if completed {
            self.finish_alignment_capacity_reclaim_if_complete();
        }
    }

    pub(super) fn finish_alignment_capacity_reclaim_if_complete(&mut self) {
        let Some(cycle) = self.alignment_capacity_reclaim.as_ref() else {
            return;
        };
        if !cycle.pending_tracks.is_empty() {
            return;
        }
        let cycle = self
            .alignment_capacity_reclaim
            .take()
            .expect("the completed aggregate reclaim cycle still exists");
        if cycle.target_revision
            != self
                .alignment_scene_publication_authority
                .desired_target_revision()
            || cycle.attempt_sequence
                != self
                    .alignment_scene_publication_authority
                    .attempt_sequence()
        {
            return;
        }
        let snapshot = match self.alignment_gpu_ledger.progress() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.alignment_gpu_publication_retry = None;
                self.session.data_status =
                    format!("Could not inspect aggregate alignment capacity: {error}");
                return;
            }
        };
        if alignment_capacity_reclaim_allows_retry(
            cycle.blocker,
            &snapshot,
            cycle.relinquished_cache_entries,
        ) {
            if let Some(retry) = self.alignment_gpu_publication_retry.as_mut()
                && retry.target_revision == cycle.target_revision
                && retry.attempt_sequence == cycle.attempt_sequence
            {
                retry.gate = AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress;
            }
            tracing::debug!(
                request_id = cycle.request_id,
                released_bytes = cycle.released_bytes,
                relinquished_cache_entries = cycle.relinquished_cache_entries,
                target_charged = cycle.target_charged,
                blocker_cleared =
                    !alignment_gpu_publication_blocker_remains(cycle.blocker, &snapshot),
                "aggregate alignment reclaim made capacity or retirement-ownership progress"
            );
            self.window.request_redraw();
            return;
        }

        self.alignment_gpu_publication_retry = None;
        let remaining_bytes = snapshot
            .charged
            .saturating_add(snapshot.reserved)
            .saturating_add(cycle.pressure.requested_bytes)
            .saturating_sub(cycle.pressure.limit_bytes);
        let message = format!(
            "Complete alignment presentation is cache-budget limited: {} MiB additional capacity is required ({} MiB configured)",
            remaining_bytes.saturating_add((1 << 20) - 1) >> 20,
            cycle.pressure.limit_bytes >> 20,
        );
        tracing::warn!(
            request_id = cycle.request_id,
            released_bytes = cycle.released_bytes,
            relinquished_cache_entries = cycle.relinquished_cache_entries,
            remaining_bytes,
            charged_bytes = snapshot.charged,
            reserved_bytes = snapshot.reserved,
            pinned_bytes = snapshot.pinned,
            target_charged = cycle.target_charged,
            "aggregate alignment reclaim exhausted every registered owner"
        );
        self.settle_alignment_navigation_gpu_budget_limited(&message);
        self.report_warning(message);
        self.mark_display_dirty("explicit display invalidation");
    }

    /// Visible read pages the presented frame leaves out for lack of GPU budget.
    pub(crate) fn alignment_gpu_deferred_read_pages(&self) -> usize {
        self.alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .and_then(|bundle| bundle.reads.as_ref())
            .map_or(0, |frame| frame.deferred_page_keys().len())
    }

    /// Records, after a publication, whether the presented read frame is partially resident,
    /// and says so in the status line. Returns the number of deferred visible pages.
    pub(super) fn record_alignment_gpu_partial_residency(&mut self) -> usize {
        let deferred_pages = self.alignment_gpu_deferred_read_pages();
        let occupied_bytes = self
            .alignment_gpu_ledger
            .progress()
            .map_or(usize::MAX, |progress| {
                progress.charged.saturating_add(progress.reserved)
            });
        let target_revision = self
            .alignment_scene_publication_authority
            .desired_target_revision();
        self.alignment_gpu_partial_top_up = alignment_gpu_partial_top_up_after_publication(
            self.alignment_gpu_partial_top_up,
            target_revision,
            deferred_pages,
            occupied_bytes,
        );
        if let Some(status) = alignment_partial_residency_status(deferred_pages) {
            self.session.data_status = status;
        }
        deferred_pages
    }

    /// Tries again to admit the visible pages a partially resident presentation left out.
    /// Runs once after each publication that made progress and then only when GPU occupancy
    /// has fallen, so a presentation that does not fit is not re-planned on every frame.
    pub(crate) fn top_up_partial_alignment_gpu_residency(&mut self) {
        let Some(top_up) = self.alignment_gpu_partial_top_up else {
            return;
        };
        if self.alignment_gpu_deferred_read_pages() == 0 {
            self.alignment_gpu_partial_top_up = None;
            return;
        }
        if self.alignment_gpu_publication_retry.is_some()
            || self.alignment_capacity_reclaim.is_some()
        {
            return;
        }
        let Ok(progress) = self.alignment_gpu_ledger.progress() else {
            return;
        };
        if !alignment_gpu_partial_top_up_is_due(
            top_up,
            self.alignment_scene_publication_authority
                .desired_target_revision(),
            progress.charged.saturating_add(progress.reserved),
        ) {
            return;
        }
        // Whatever this attempt publishes re-arms the record (or clears it when complete). If
        // it publishes nothing, the unchanged record below must not fire again at once.
        self.alignment_gpu_partial_top_up = Some(AlignmentGpuPartialTopUp {
            fruitless_attempts: top_up.fruitless_attempts.saturating_add(1),
            occupied_bytes: progress.charged.saturating_add(progress.reserved),
            ..top_up
        });
        self.alignment_gpu_top_up_in_progress = true;
        self.prepare_alignment_scene_frame_atomic();
        self.alignment_gpu_top_up_in_progress = false;
    }

    /// Polls a typed foreground refusal without converting the render loop into a retry clock.
    /// The exact latest target/attempt is retried once after ledger progress; unchanged epochs do
    /// nothing, and a newer attempt discards stale retry authority.
    pub(crate) fn retry_deferred_alignment_gpu_publication(&mut self) {
        let Some(retry) = self.alignment_gpu_publication_retry else {
            return;
        };
        let snapshot = match self.alignment_gpu_ledger.progress() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.alignment_gpu_publication_retry = None;
                self.session.data_status =
                    format!("Could not inspect deferred alignment GPU capacity: {error}");
                return;
            }
        };
        match alignment_gpu_publication_retry_poll(
            retry,
            self.alignment_scene_publication_authority
                .desired_target_revision(),
            self.alignment_scene_publication_authority
                .attempt_sequence(),
            &snapshot,
        ) {
            AlignmentGpuPublicationRetryPoll::Superseded => {
                self.alignment_gpu_publication_retry = None;
                self.alignment_capacity_reclaim = None;
            }
            AlignmentGpuPublicationRetryPoll::Waiting(updated) => {
                self.alignment_gpu_publication_retry = Some(updated);
            }
            AlignmentGpuPublicationRetryPoll::Ready => {
                tracing::debug!(
                    blocker = ?retry.blocker,
                    "retrying alignment GPU publication after ledger progress"
                );
                self.alignment_gpu_publication_retry = None;
                self.alignment_capacity_reclaim = None;
                self.prepare_alignment_scene_frame_atomic();
            }
        }
    }

    /// Gives a foreground source retry priority over every optional GPU page. Published frame
    /// pins remain intact, while complete-scene residency, pending prefetch authority, idle frame
    /// buffers, sparse tables, and unpinned LRU pages yield under one shared-ledger headroom goal.
    /// The returned epoch is recorded with the exact retry so an unchanged refusal cannot spin.
    pub(crate) fn yield_alignment_gpu_speculation_for_source_pressure(
        &mut self,
        pressure: LedgerError,
    ) -> std::result::Result<LedgerProgressEpoch, LedgerError> {
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_speculation_gate
            .suppress_for_foreground_pressure();

        if !self.alignment_gpu_ledger_is_private()
            && let Some(pressure) = alignment_source_pressure_gpu_budget(pressure)
        {
            let baseline = self.alignment_gpu_ledger.progress()?;
            let occupied = baseline.charged.saturating_add(baseline.reserved);
            let goal = GpuReclaimGoal::new(pressure, occupied)
                .with_additional_headroom_above_floor(pressure.requested_bytes, baseline.pinned);
            if goal.is_feasible() && !goal.is_satisfied_at(occupied) {
                let reads = self.alignment_gpu_renderer.reclaim_for_goal(
                    &self.alignment_gpu_ledger,
                    goal,
                    &[],
                )?;
                if !reads.headroom_satisfied {
                    let _ = self.alignment_summary_gpu_renderer.reclaim_for_goal(
                        &self.alignment_gpu_ledger,
                        goal,
                        &[],
                    )?;
                }
            }
        }

        Ok(self
            .session
            .alignment_scene_ledger
            .progress()?
            .progress_epoch())
    }

    /// Coordinates one composable failure-time reclaim goal across both GPU cache owners. Every
    /// full-scene residency lease is speculative and retires together, while published frames and
    /// both upcoming foreground key sets remain pinned/protected. Reads and summaries participate
    /// in the same deterministic pass because the retry is one combined candidate, not a request
    /// from either renderer independently.
    pub(super) fn reclaim_alignment_gpu_caches_for_foreground(
        &mut self,
        pressure: GpuBudgetPressure,
        protected_read_pages: &[AlignmentPageKey],
        protected_summary_pages: &[AlignmentSummaryGpuPageKey],
    ) -> std::result::Result<AlignmentGpuReclaimOutcome, LedgerError> {
        let baseline = self.alignment_gpu_ledger.progress()?;
        let baseline_occupied = baseline.charged.saturating_add(baseline.reserved);
        let goal = GpuReclaimGoal::new(pressure, baseline_occupied)
            .with_additional_headroom_above_floor(pressure.requested_bytes, baseline.pinned);
        if !goal.is_feasible() {
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_speculation_gate
                .suppress_for_foreground_pressure();
            tracing::warn!(
                pressure_cause = ?pressure.cause,
                requested_bytes = pressure.requested_bytes,
                retirement_credit_bytes = pressure.retirement_credit_bytes,
                charged_bytes_at_failure = pressure.charged_bytes_at_failure,
                reserved_bytes_at_failure = pressure.reserved_bytes_at_failure,
                allocation_limit_bytes = pressure.limit_bytes,
                required_reclaim_bytes = goal.required_reclaim_bytes(),
                baseline_occupied_bytes = goal.baseline_occupied_bytes,
                "alignment GPU candidate is intrinsically larger than reclaimable capacity"
            );
            return Ok(AlignmentGpuReclaimOutcome::for_goal(
                goal,
                baseline_occupied,
                baseline.pinned,
                0,
                0,
            ));
        }

        let retired_residency = self.alignment_gpu_residency_bundle.take();
        let retired_read_residency = usize::from(
            retired_residency
                .as_ref()
                .is_some_and(|bundle| bundle._reads.is_some()),
        );
        let retired_summary_residencies = usize::from(
            retired_residency
                .as_ref()
                .is_some_and(|bundle| bundle._summaries.is_some()),
        );
        drop(retired_residency);
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_speculation_gate
            .suppress_for_foreground_pressure();

        let reads = self.alignment_gpu_renderer.reclaim_for_goal(
            &self.alignment_gpu_ledger,
            goal,
            protected_read_pages,
        )?;
        let summaries = if reads.headroom_satisfied {
            None
        } else {
            Some(self.alignment_summary_gpu_renderer.reclaim_for_goal(
                &self.alignment_gpu_ledger,
                goal,
                protected_summary_pages,
            )?)
        };
        let final_snapshot = self.alignment_gpu_ledger.progress()?;
        let final_occupied = final_snapshot
            .charged
            .saturating_add(final_snapshot.reserved);
        let outcome = AlignmentGpuReclaimOutcome::for_goal(
            goal,
            final_occupied,
            final_snapshot.pinned,
            retired_read_residency,
            retired_summary_residencies,
        );
        let headroom_satisfied = matches!(outcome, AlignmentGpuReclaimOutcome::Satisfied { .. });
        tracing::debug!(
            pressure_cause = ?pressure.cause,
            retired_read_residency,
            retired_summary_residencies,
            requested_bytes = pressure.requested_bytes,
            retirement_credit_bytes = pressure.retirement_credit_bytes,
            charged_bytes_at_failure = pressure.charged_bytes_at_failure,
            reserved_bytes_at_failure = pressure.reserved_bytes_at_failure,
            allocation_limit_bytes = pressure.limit_bytes,
            required_reclaim_bytes = goal.required_reclaim_bytes(),
            read_reclaimed_pages = reads.reclaimed_pages(),
            read_released_bytes = reads.released_ledger_bytes(),
            summary_reclaimed_pages = summaries
                .as_ref()
                .map_or(0, GpuReclaimReport::reclaimed_pages),
            summary_released_bytes = summaries
                .as_ref()
                .map_or(0, GpuReclaimReport::released_ledger_bytes),
            headroom_satisfied,
            "coordinated alignment GPU foreground reclaim"
        );
        Ok(outcome)
    }

    pub(crate) fn apply_live_cache_budget(&mut self, cache_budget_mib: usize) {
        let previous_budget_bytes = self.alignment_gpu_ledger.progress().map_or(
            self.session.alignment_scene_budget.budget_bytes,
            |snapshot| snapshot.budget,
        );
        let total_bytes = cache_budget_mib.saturating_mul(1024 * 1024).max(1);
        self.refresh_alignment_scene_budget();
        let current_budget_bytes = self
            .alignment_gpu_ledger
            .progress()
            .map_or(total_bytes, |snapshot| snapshot.budget);
        if current_budget_bytes < previous_budget_bytes {
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_speculation_gate.budget_bytes = current_budget_bytes;
            self.alignment_gpu_speculation_gate
                .suppress_for_foreground_pressure();
            if let Ok(snapshot) = self.alignment_gpu_ledger.progress() {
                let pressure = GpuBudgetPressure::from_budget_failure(
                    0,
                    snapshot.charged,
                    snapshot.reserved,
                    current_budget_bytes,
                );
                let goal = GpuReclaimGoal::new(
                    pressure,
                    snapshot.charged.saturating_add(snapshot.reserved),
                );
                if goal.required_reclaim_bytes() > 0 {
                    let _ = self.alignment_gpu_renderer.reclaim_for_goal(
                        &self.alignment_gpu_ledger,
                        goal,
                        &[],
                    );
                    let _ = self.alignment_summary_gpu_renderer.reclaim_for_goal(
                        &self.alignment_gpu_ledger,
                        goal,
                        &[],
                    );
                }
            }
        } else if current_budget_bytes > previous_budget_bytes {
            self.alignment_gpu_speculation_gate.budget_bytes = current_budget_bytes;
            self.alignment_gpu_speculation_gate.phase = AlignmentGpuSpeculationPhase::Eligible;
            let (reads, summaries) = self
                .alignment_presented_epoch
                .as_ref()
                .and_then(|epoch| epoch.gpu_bundle.as_ref())
                .map_or((false, false), |bundle| {
                    (bundle.reads.is_some(), bundle.summaries.is_some())
                });
            self.alignment_gpu_prefetch.reset(reads, summaries);
            self.window.request_redraw();
        }
    }

    pub(crate) fn aggregate_cache_budget_bytes(&self) -> usize {
        self.session
            .settings
            .cache_budget_mib
            .saturating_mul(1024 * 1024)
            .max(1)
    }

    pub(super) fn externally_reserved_gpu_cache_bytes(&self) -> (usize, usize) {
        (
            self.context_scene_gpu_renderer.resident_bytes(),
            self.rectangle_renderer.resident_bytes(),
        )
    }

    pub(crate) fn alignment_ledger_occupied_bytes(&self) -> usize {
        let occupied = |ledger: &AllocationLedger| {
            ledger.progress().map_or(0, |progress| {
                progress.charged.saturating_add(progress.reserved)
            })
        };
        let scene = occupied(&self.session.alignment_scene_ledger);
        if self.alignment_gpu_ledger_is_private() {
            scene.saturating_add(occupied(&self.alignment_gpu_ledger))
        } else {
            scene
        }
    }

    /// Whether GPU pages are charged to their own ledger (the bounded memory policy) rather
    /// than to the ledger shared with the source engines (the strict policy).
    pub(crate) fn alignment_gpu_ledger_is_private(&self) -> bool {
        self.session.alignment_scene_ledger.is_bounded()
    }

    /// Reconciles the shared alignment ledger and the two not-yet-ledger-native GPU caches under
    /// one configured steady-state ceiling. Context and generic slabs receive explicit
    /// non-borrowing reservations: their measured resident bytes are subtracted from the ledger
    /// allowance, while their next admission allowance subtracts current ledger occupancy. This
    /// prevents the setting from being multiplied across independent pools until those renderers
    /// participate in move-only ledger transactions directly.
    pub(crate) fn refresh_alignment_scene_budget(&mut self) {
        let total_budget_bytes = self.aggregate_cache_budget_bytes();
        let (context_bytes, rectangle_bytes) = self.externally_reserved_gpu_cache_bytes();
        let external_reservation = context_bytes.saturating_add(rectangle_bytes);
        let alignment_budget_bytes = total_budget_bytes
            .saturating_sub(external_reservation)
            .max(1);
        self.session.alignment_scene_budget.budget_bytes = alignment_budget_bytes;
        if let Err(error) = self.session.alignment_scene_ledger.set_limits(
            alignment_budget_bytes,
            total_budget_bytes,
            total_budget_bytes,
        ) {
            self.report_error(format!(
                "Could not apply alignment resource limits: {error}"
            ));
        }
        if self.alignment_gpu_ledger_is_private() {
            let gpu_budget_bytes = alignment_gpu_share_bytes(alignment_budget_bytes);
            if let Err(error) = self.alignment_gpu_ledger.set_limits(
                gpu_budget_bytes,
                gpu_budget_bytes,
                total_budget_bytes,
            ) {
                self.report_error(format!(
                    "Could not apply alignment GPU resource limits: {error}"
                ));
            }
        }

        let allowance = NonZeroUsize::new(alignment_budget_bytes)
            .expect("the effective alignment budget is non-zero");
        let mut first_error = None;
        for track in &mut self.session.alignment_tracks {
            let Some(engine) = &track.scene_engine else {
                continue;
            };
            if track.scene_budget_bytes == allowance {
                continue;
            }
            match engine.try_reduce_budget(alignment_budget_bytes) {
                Ok(()) => {
                    track.scene_budget_bytes = allowance;
                    track.scene_budget_retry = None;
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| error.to_string());
                }
            }
        }

        self.context_lane_gpu_budget_bytes = total_budget_bytes
            .saturating_sub(self.alignment_ledger_occupied_bytes())
            .saturating_sub(context_bytes)
            .saturating_sub(self.rectangle_renderer.primary_resident_bytes());
        if let Ok(progress) = self.session.alignment_scene_ledger.progress() {
            self.session.alignment_scene_budget = AlignmentSceneBudgetSnapshot::from_progress(
                progress,
                self.session
                    .alignment_scene_budget
                    .refreshes
                    .saturating_add(1),
            );
        }
        if let Some(error) = first_error {
            self.report_error(format!("Could not apply cache budget: {error}"));
        }
    }
}

/// The GPU owner's fixed share of the alignment cache budget under the bounded memory policy.
pub(crate) fn alignment_gpu_share_bytes(alignment_budget_bytes: usize) -> usize {
    AllocationOwnerShares::default()
        .share_of(alignment_budget_bytes, AllocationOwner::Gpu)
        .max(1)
}

/// The ledger GPU pages are charged to. The strict policy shares the scene ledger, exactly as
/// before. The bounded policy gives the GPU caches a private strict ledger sized to their share:
/// they evict their own least-recently-used pages and never contend with CPU scene work.
pub(crate) fn alignment_gpu_ledger_for(
    scene_ledger: &AllocationLedger,
    alignment_budget_bytes: usize,
) -> AllocationLedger {
    if scene_ledger.is_bounded() {
        let share = alignment_gpu_share_bytes(alignment_budget_bytes);
        AllocationLedger::new_with_transient_budget(share, share, alignment_budget_bytes)
    } else {
        scene_ledger.clone()
    }
}
