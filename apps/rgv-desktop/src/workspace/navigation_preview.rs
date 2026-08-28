use super::super::*;

pub(crate) const NAVIGATION_QUERY_IDLE_DELAY: Duration = Duration::from_millis(50);
pub(crate) const NAVIGATION_QUERY_MAX_CADENCE: Duration = Duration::from_millis(150);
pub(crate) const NAVIGATION_PREVIEW_NOTICE_DELAY: Duration = Duration::from_millis(150);
pub(crate) const MAX_PROVISIONAL_HORIZON_RECORDS: usize = 4_000;
pub(crate) const MAX_PROVISIONAL_HORIZON_BYTES: usize = 24 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NavigationHorizonPreviewStats {
    pub(crate) eligibility_checks: u64,
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) budget_fallbacks: u64,
    pub(crate) last_build_micros: u64,
    pub(crate) last_rectangle_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NavigationPreviewKind {
    Texture,
    CompleteHorizon,
}

impl NavigationPreviewKind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Texture => "texture",
            Self::CompleteHorizon => "complete_horizon",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NavigationPresentationPhase {
    Settled,
    Preview,
    CheckingWithoutPreview,
    ReadyToCommit,
    Committing,
}

#[derive(Clone, Debug)]
pub(crate) struct NavigationPreviewSource {
    pub(crate) generation: u64,
    pub(crate) interval: GenomicInterval,
    pub(crate) exact_start: f64,
    pub(crate) span: f64,
    pub(crate) content_revision: u64,
    pub(crate) layout_revision: u64,
    pub(crate) contract_revision: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct NavigationPresentation {
    pub(crate) phase: NavigationPresentationPhase,
    pub(crate) source: Option<NavigationPreviewSource>,
    pub(crate) target_generation: u64,
    pub(crate) target_interval: GenomicInterval,
    pub(crate) target_exact_start: f64,
    pub(crate) target_span: f64,
    pub(crate) started_at: Instant,
    pub(crate) notice_visible: bool,
    pub(crate) preview_kind: Option<NavigationPreviewKind>,
    pub(crate) invalidation_reason: Option<String>,
    pub(crate) source_status: String,
    pub(crate) committed_generation: Option<u64>,
    pub(crate) committed_interval: Option<GenomicInterval>,
    pub(crate) committed_exact_start: Option<f64>,
    pub(crate) committed_span: Option<f64>,
    pub(crate) committed_content_revision: u64,
    pub(crate) committed_layout_revision: u64,
    pub(crate) committed_contract_revision: u64,
}

impl NavigationPresentation {
    pub(crate) fn initial(interval: GenomicInterval, generation: u64, now: Instant) -> Self {
        Self {
            phase: NavigationPresentationPhase::CheckingWithoutPreview,
            source: None,
            target_generation: generation,
            target_interval: interval,
            target_exact_start: interval.start() as f64,
            target_span: interval.len() as f64,
            started_at: now,
            notice_visible: false,
            preview_kind: None,
            invalidation_reason: Some("initial load has no committed preview".to_owned()),
            source_status: String::new(),
            committed_generation: None,
            committed_interval: None,
            committed_exact_start: None,
            committed_span: None,
            committed_content_revision: 0,
            committed_layout_revision: 0,
            committed_contract_revision: 0,
        }
    }

    pub(crate) fn begin_navigation(
        &mut self,
        target_viewport: Viewport,
        target_generation: u64,
        layout_revision: u64,
        contract_revision: u64,
        source_status: &str,
        now: Instant,
    ) {
        let target_interval = target_viewport.interval();
        let committed_source = self.committed_interval.and_then(|interval| {
            (self.committed_content_revision > 0
                && self.committed_layout_revision == layout_revision
                && self.committed_contract_revision == contract_revision)
                .then_some(NavigationPreviewSource {
                    generation: self.committed_generation.unwrap_or_default(),
                    interval,
                    exact_start: self
                        .committed_exact_start
                        .unwrap_or(interval.start() as f64),
                    span: self.committed_span.unwrap_or(interval.len() as f64),
                    content_revision: self.committed_content_revision,
                    layout_revision: self.committed_layout_revision,
                    contract_revision: self.committed_contract_revision,
                })
        });
        // Always preserve the original committed source across superseded
        // targets. If a same-contig jump temporarily moves fully away from it,
        // a later gesture update may still move back into useful overlap.
        let source_candidate = if self.phase == NavigationPresentationPhase::Settled {
            committed_source
        } else {
            self.source.clone().or(committed_source)
        };
        let candidate_has_matching_contract = source_candidate.as_ref().is_some_and(|source| {
            source.layout_revision == layout_revision
                && source.contract_revision == contract_revision
        });
        let source = source_candidate.filter(|source| {
            candidate_has_matching_contract
                && navigation_preview_has_useful_overlap(source, target_viewport)
        });
        let is_new_transaction = self.phase == NavigationPresentationPhase::Settled
            || matches!(
                self.phase,
                NavigationPresentationPhase::Committing
                    | NavigationPresentationPhase::ReadyToCommit
            );
        if is_new_transaction {
            self.started_at = now;
            self.notice_visible = false;
            self.source_status = source_status.to_owned();
        }
        self.source = source;
        self.target_generation = target_generation;
        self.target_interval = target_interval;
        self.target_exact_start = target_viewport.exact_start();
        self.target_span = target_viewport.span();
        self.phase = if self.source.is_some() {
            NavigationPresentationPhase::Preview
        } else {
            NavigationPresentationPhase::CheckingWithoutPreview
        };
        self.preview_kind = self.source.as_ref().map(|_| NavigationPreviewKind::Texture);
        self.invalidation_reason = self.source.is_none().then(|| {
            if self
                .committed_interval
                .is_some_and(|interval| interval.contig() != target_interval.contig())
            {
                "target is on a different contig".to_owned()
            } else if self.committed_layout_revision != layout_revision {
                "workspace layout changed".to_owned()
            } else if self.committed_contract_revision != contract_revision {
                "genomic presentation contract changed".to_owned()
            } else if candidate_has_matching_contract {
                "target has no useful overlap with committed preview".to_owned()
            } else {
                "no committed preview is available".to_owned()
            }
        });
    }

    pub(crate) fn reveal_notice_if_due(&mut self, now: Instant) -> bool {
        if self.notice_visible
            || self.phase == NavigationPresentationPhase::Settled
            || now.saturating_duration_since(self.started_at) < NAVIGATION_PREVIEW_NOTICE_DELAY
        {
            return false;
        }
        self.notice_visible = true;
        true
    }

    pub(crate) fn invalidate_preview(&mut self, reason: impl Into<String>) {
        if matches!(
            self.phase,
            NavigationPresentationPhase::Preview | NavigationPresentationPhase::ReadyToCommit
        ) {
            self.phase = NavigationPresentationPhase::CheckingWithoutPreview;
        }
        self.source = None;
        self.preview_kind = None;
        self.invalidation_reason = Some(reason.into());
    }

    pub(crate) fn require_captured_source_revision(&mut self, captured_revision: Option<u64>) {
        if self
            .source
            .as_ref()
            .is_some_and(|source| captured_revision != Some(source.content_revision))
        {
            self.invalidate_preview("committed preview texture capture is unavailable");
        }
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

    pub(crate) fn finish_commit(&mut self, layout_revision: u64, contract_revision: u64) -> bool {
        if self.phase != NavigationPresentationPhase::Committing {
            return false;
        }
        self.committed_generation = Some(self.target_generation);
        self.committed_interval = Some(self.target_interval);
        self.committed_exact_start = Some(self.target_exact_start);
        self.committed_span = Some(self.target_span);
        self.committed_content_revision = self.committed_content_revision.saturating_add(1);
        self.committed_layout_revision = layout_revision;
        self.committed_contract_revision = contract_revision;
        self.phase = NavigationPresentationPhase::Settled;
        self.source = None;
        self.notice_visible = false;
        self.preview_kind = None;
        self.invalidation_reason = None;
        true
    }

    pub(crate) fn record_settled_capture(
        &mut self,
        viewport: Viewport,
        generation: u64,
        content_revision: u64,
        layout_revision: u64,
        contract_revision: u64,
    ) {
        if self.phase != NavigationPresentationPhase::Settled {
            return;
        }
        let interval = viewport.interval();
        self.target_interval = interval;
        self.target_exact_start = viewport.exact_start();
        self.target_span = viewport.span();
        self.target_generation = generation;
        self.committed_interval = Some(interval);
        self.committed_exact_start = Some(viewport.exact_start());
        self.committed_span = Some(viewport.span());
        self.committed_generation = Some(generation);
        self.committed_content_revision = content_revision.max(1);
        self.committed_layout_revision = layout_revision;
        self.committed_contract_revision = contract_revision;
        self.invalidation_reason = None;
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

    pub(crate) const fn shows_texture_preview(&self) -> bool {
        matches!(
            self.phase,
            NavigationPresentationPhase::Preview | NavigationPresentationPhase::ReadyToCommit
        ) && matches!(self.preview_kind, Some(NavigationPreviewKind::Texture))
    }

    pub(crate) const fn shows_retained_preview(&self) -> bool {
        self.shows_texture_preview()
            || (matches!(
                self.phase,
                NavigationPresentationPhase::Preview | NavigationPresentationPhase::ReadyToCommit
            ) && matches!(
                self.preview_kind,
                Some(NavigationPreviewKind::CompleteHorizon)
            ))
    }

    pub(crate) fn prefer_complete_horizon(&mut self) -> bool {
        if self.source.is_none() || !matches!(self.phase, NavigationPresentationPhase::Preview) {
            return false;
        }
        self.preview_kind = Some(NavigationPreviewKind::CompleteHorizon);
        true
    }

    pub(crate) const fn permits_staged_display(&self) -> bool {
        matches!(
            self.phase,
            NavigationPresentationPhase::Committing | NavigationPresentationPhase::Settled
        )
    }

    pub(crate) fn age(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.started_at)
    }
}

pub(crate) fn navigation_preview_has_useful_overlap(
    source: &NavigationPreviewSource,
    target: Viewport,
) -> bool {
    if source.interval.contig() != target.interval().contig()
        || !source.exact_start.is_finite()
        || !source.span.is_finite()
        || source.span <= 0.0
    {
        return false;
    }
    let target_start = target.exact_start();
    let target_span = target.span();
    if !target_start.is_finite() || !target_span.is_finite() || target_span <= 0.0 {
        return false;
    }
    let source_end = source.exact_start + source.span;
    let target_end = target_start + target_span;
    let overlap = source_end.min(target_end) - source.exact_start.max(target_start);
    overlap.is_finite() && overlap > 0.0 && overlap / target_span * target.pixel_width() >= 1.0
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
    /// Pointer-driven pans remain one presentation transaction until release,
    /// even when cadence queries complete while the pointer is still down.
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
    pub(crate) preview_frames: u64,
    pub(crate) neutral_frames_after_commit: u64,
    pub(crate) current_transaction_preview_frames: u64,
    pub(crate) last_transaction_preview_frames: u64,
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
        // Mouse wheels do not consistently emit an Ended phase on every
        // platform. An idle dispatch is therefore also the implicit end of a
        // non-pointer-held gesture, so the next independent burst receives a
        // fresh 50 ms coalescing window rather than inheriting an old cadence.
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

    pub(crate) fn begin_transaction(&mut self) {
        self.current_transaction_preview_frames = 0;
    }

    pub(crate) fn record_superseded_target(&mut self) {
        self.cancelled_targets = self.cancelled_targets.saturating_add(1);
    }

    pub(crate) fn record_preview_frame(&mut self, neutral_after_commit: bool) {
        self.preview_frames = self.preview_frames.saturating_add(1);
        self.current_transaction_preview_frames =
            self.current_transaction_preview_frames.saturating_add(1);
        if neutral_after_commit {
            self.neutral_frames_after_commit = self.neutral_frames_after_commit.saturating_add(1);
        }
    }

    pub(crate) fn record_commit(&mut self, elapsed: Duration) {
        self.committed_targets = self.committed_targets.saturating_add(1);
        self.last_transaction_preview_frames = self.current_transaction_preview_frames;
        self.current_transaction_preview_frames = 0;
        self.last_commit_micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
    }
}
