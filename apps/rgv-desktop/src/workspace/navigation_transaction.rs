use crate::app::status_activity::StatusActivity;
use rgv_core::{GenomicInterval, Viewport};
use std::time::{Duration, Instant};

pub(crate) const NAVIGATION_QUERY_IDLE_DELAY: Duration = Duration::from_millis(50);
pub(crate) const NAVIGATION_QUERY_MAX_CADENCE: Duration = Duration::from_millis(150);
pub(crate) const NAVIGATION_NOTICE_DELAY: Duration = Duration::from_millis(150);

/// Control-plane state for one viewport target. This state does not own pixels: alignment and
/// contextual lane scenes continue rendering independently under their live cameras while source
/// work is acquired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NavigationPresentationPhase {
    Settled,
    Acquiring,
    ReadyToCommit,
    Committing,
}

#[derive(Clone, Debug)]
pub(crate) struct NavigationPresentation {
    pub(crate) phase: NavigationPresentationPhase,
    pub(crate) target_generation: u64,
    pub(crate) target_interval: GenomicInterval,
    pub(crate) target_exact_start: f64,
    pub(crate) target_span: f64,
    pub(crate) started_at: Instant,
    pub(crate) notice_visible: bool,
    pub(crate) invalidation_reason: Option<String>,
    pub(crate) source_status: String,
    /// Whether `source_status` described background work when the transaction began.
    pub(crate) source_activity: StatusActivity,
    /// Monotonic semantic presentation epoch. This is consumed by interaction provenance and is
    /// unrelated to any retained texture.
    pub(crate) committed_content_revision: u64,
}

impl NavigationPresentation {
    pub(crate) fn initial(interval: GenomicInterval, generation: u64, now: Instant) -> Self {
        Self {
            phase: NavigationPresentationPhase::Acquiring,
            target_generation: generation,
            target_interval: interval,
            target_exact_start: interval.start() as f64,
            target_span: interval.len() as f64,
            started_at: now,
            notice_visible: false,
            invalidation_reason: Some("initial scene publication is pending".to_owned()),
            source_status: String::new(),
            source_activity: StatusActivity::Idle,
            committed_content_revision: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn begin_navigation(
        &mut self,
        target_viewport: Viewport,
        target_generation: u64,
        layout_revision: u64,
        contract_revision: u64,
        source_status: &str,
        now: Instant,
    ) {
        self.begin_navigation_with_activity(
            target_viewport,
            target_generation,
            layout_revision,
            contract_revision,
            source_status,
            StatusActivity::Idle,
            now,
        );
    }

    /// As [`Self::begin_navigation`], also recording whether the status line that the
    /// transaction preserves described background work.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin_navigation_with_activity(
        &mut self,
        target_viewport: Viewport,
        target_generation: u64,
        _layout_revision: u64,
        _contract_revision: u64,
        source_status: &str,
        source_activity: StatusActivity,
        now: Instant,
    ) {
        let is_new_transaction = matches!(
            self.phase,
            NavigationPresentationPhase::Settled
                | NavigationPresentationPhase::Committing
                | NavigationPresentationPhase::ReadyToCommit
        );
        if is_new_transaction {
            self.started_at = now;
            self.notice_visible = false;
            self.source_status = source_status.to_owned();
            self.source_activity = source_activity;
        }
        self.target_generation = target_generation;
        self.target_interval = target_viewport.interval();
        self.target_exact_start = target_viewport.exact_start();
        self.target_span = target_viewport.span();
        self.phase = NavigationPresentationPhase::Acquiring;
        self.invalidation_reason = None;
    }

    pub(crate) fn reveal_notice_if_due(&mut self, now: Instant) -> bool {
        if self.notice_visible
            || self.phase == NavigationPresentationPhase::Settled
            || now.saturating_duration_since(self.started_at) < NAVIGATION_NOTICE_DELAY
        {
            return false;
        }
        self.notice_visible = true;
        true
    }

    pub(crate) fn invalidate_contract(&mut self, reason: impl Into<String>) {
        if self.phase != NavigationPresentationPhase::Settled {
            self.phase = NavigationPresentationPhase::Acquiring;
        }
        self.invalidation_reason = Some(reason.into());
    }

    pub(crate) fn mark_ready_to_commit(&mut self) {
        if self.phase != NavigationPresentationPhase::Settled {
            self.phase = NavigationPresentationPhase::ReadyToCommit;
        }
    }

    pub(crate) fn begin_commit(&mut self) -> bool {
        if self.phase != NavigationPresentationPhase::ReadyToCommit {
            return false;
        }
        self.phase = NavigationPresentationPhase::Committing;
        true
    }

    pub(crate) fn finish_commit(&mut self, _layout_revision: u64, _contract_revision: u64) -> bool {
        if self.phase != NavigationPresentationPhase::Committing {
            return false;
        }
        self.committed_content_revision = self.committed_content_revision.saturating_add(1).max(1);
        self.phase = NavigationPresentationPhase::Settled;
        self.notice_visible = false;
        self.invalidation_reason = None;
        true
    }

    pub(crate) fn is_interactive(&self) -> bool {
        self.phase == NavigationPresentationPhase::Settled
    }

    pub(crate) fn is_unsettled(&self) -> bool {
        self.phase != NavigationPresentationPhase::Settled
    }

    pub(crate) fn targets_viewport(&self, viewport: Viewport) -> bool {
        self.target_interval == viewport.interval()
            && self.target_exact_start.to_bits() == viewport.exact_start().to_bits()
            && self.target_span.to_bits() == viewport.span().to_bits()
    }

    pub(crate) fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started_at)
    }
}

pub(crate) const fn navigation_target_ready_for_commit(
    phase: NavigationPresentationPhase,
    hold_commit_until_end: bool,
    data_settled: bool,
    row_layouts_settled: bool,
) -> bool {
    matches!(phase, NavigationPresentationPhase::Acquiring)
        && !hold_commit_until_end
        && data_settled
        && row_layouts_settled
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NavigationTargetFingerprint {
    pub(crate) interval: GenomicInterval,
    pub(crate) pixel_width_bits: u64,
    pub(crate) exact_start_bits: u64,
    pub(crate) span_bits: u64,
}

impl NavigationTargetFingerprint {
    pub(crate) fn from_viewport(viewport: Viewport) -> Self {
        Self {
            interval: viewport.interval(),
            pixel_width_bits: viewport.pixel_width().to_bits(),
            exact_start_bits: viewport.exact_start().to_bits(),
            span_bits: viewport.span().to_bits(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NavigationQueryTarget {
    pub(crate) fingerprint: NavigationTargetFingerprint,
    pub(crate) generation: u64,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NavigationQuerySchedule {
    pub(crate) gesture_active: bool,
    /// Pointer-driven pans remain one presentation transaction until release, even when cadence
    /// queries complete while the pointer is still down.
    pub(crate) hold_commit_until_end: bool,
    pub(crate) pending: Option<NavigationQueryTarget>,
    pub(crate) first_pending_at: Option<Instant>,
    pub(crate) last_input_at: Option<Instant>,
    pub(crate) last_dispatched: Option<NavigationQueryTarget>,
    pub(crate) last_dispatch_at: Option<Instant>,
    pub(crate) input_updates: u64,
    pub(crate) coalesced_updates: u64,
    pub(crate) dispatched_targets: u64,
    pub(crate) gesture_end_dispatches: u64,
    pub(crate) cancelled_targets: u64,
    pub(crate) committed_targets: u64,
    pub(crate) last_commit_micros: u64,
}

impl NavigationQuerySchedule {
    pub(crate) fn update(
        &mut self,
        target: NavigationQueryTarget,
        now: Instant,
        gesture_active: bool,
        hold_commit_until_end: bool,
    ) {
        self.input_updates = self.input_updates.saturating_add(1);
        if gesture_active && !self.gesture_active {
            self.last_dispatch_at = None;
        }
        if self
            .pending
            .is_some_and(|pending| pending.fingerprint != target.fingerprint)
        {
            self.coalesced_updates = self.coalesced_updates.saturating_add(1);
        }
        if self.pending.is_none() {
            self.first_pending_at = Some(now);
        }
        self.pending = Some(target);
        self.last_input_at = Some(now);
        self.gesture_active |= gesture_active;
        self.hold_commit_until_end |= hold_commit_until_end;
    }

    pub(crate) fn take_due(&mut self, now: Instant) -> Option<NavigationQueryTarget> {
        self.pending?;
        let idle_due = self
            .last_input_at
            .is_some_and(|last| now.saturating_duration_since(last) >= NAVIGATION_QUERY_IDLE_DELAY);
        let cadence_origin = self.last_dispatch_at.or(self.first_pending_at);
        let cadence_due = self.gesture_active
            && cadence_origin.is_some_and(|origin| {
                now.saturating_duration_since(origin) >= NAVIGATION_QUERY_MAX_CADENCE
            });
        if idle_due && !self.hold_commit_until_end {
            self.gesture_active = false;
        }
        (idle_due || cadence_due)
            .then(|| self.take_pending(now, false))
            .flatten()
    }

    pub(crate) fn take_immediate(
        &mut self,
        now: Instant,
        gesture_end: bool,
    ) -> Option<NavigationQueryTarget> {
        if gesture_end {
            self.gesture_active = false;
            self.hold_commit_until_end = false;
        }
        self.take_pending(now, gesture_end)
    }

    fn take_pending(&mut self, now: Instant, gesture_end: bool) -> Option<NavigationQueryTarget> {
        let pending = self.pending.take()?;
        self.first_pending_at = None;
        self.last_input_at = None;
        if self.last_dispatched == Some(pending) {
            self.coalesced_updates = self.coalesced_updates.saturating_add(1);
            return None;
        }
        self.last_dispatched = Some(pending);
        self.last_dispatch_at = Some(now);
        self.dispatched_targets = self.dispatched_targets.saturating_add(1);
        if gesture_end {
            self.gesture_end_dispatches = self.gesture_end_dispatches.saturating_add(1);
        }
        Some(pending)
    }

    pub(crate) fn cancel(&mut self) {
        if self.pending.is_some() || self.gesture_active || self.hold_commit_until_end {
            self.cancelled_targets = self.cancelled_targets.saturating_add(1);
        }
        self.gesture_active = false;
        self.hold_commit_until_end = false;
        self.pending = None;
        self.first_pending_at = None;
        self.last_input_at = None;
    }

    pub(crate) fn record_superseded_target(&mut self) {
        self.cancelled_targets = self.cancelled_targets.saturating_add(1);
    }

    pub(crate) fn record_commit(&mut self, elapsed: Duration) {
        self.committed_targets = self.committed_targets.saturating_add(1);
        self.last_commit_micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    }
}
