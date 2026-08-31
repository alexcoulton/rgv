use super::super::*;

const ALIGNMENT_NAVIGATION_JOURNAL_CAPACITY: usize = 64;

/// The component which currently owns forward progress for an alignment target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AlignmentNavigationOwner {
    DesktopQueue,
    SourceWorker,
    SourcePressure,
    GpuCandidate,
    GpuPressure,
}

/// Constant-size readiness state for one product in the alignment dependency graph. Reference
/// and contextual products are reported even though they are explicitly independent of the core
/// read-navigation commit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AlignmentNavigationProductState {
    #[default]
    Pending,
    NotRequired,
    Independent,
    IndependentPending,
    IndependentReady,
    IndependentFailed,
    Queued,
    Running,
    Ready,
    Pressure,
    BudgetLimited,
    Unavailable,
    Failed,
    Superseded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AlignmentNavigationProducts {
    pub(crate) source: AlignmentNavigationProductState,
    pub(crate) row_atlas: AlignmentNavigationProductState,
    pub(crate) semantic_pages: AlignmentNavigationProductState,
    pub(crate) gpu: AlignmentNavigationProductState,
    pub(crate) reference: AlignmentNavigationProductState,
    pub(crate) contextual_lanes: AlignmentNavigationProductState,
}

impl Default for AlignmentNavigationProducts {
    fn default() -> Self {
        Self {
            source: AlignmentNavigationProductState::NotRequired,
            row_atlas: AlignmentNavigationProductState::NotRequired,
            semantic_pages: AlignmentNavigationProductState::NotRequired,
            gpu: AlignmentNavigationProductState::NotRequired,
            reference: AlignmentNavigationProductState::NotRequired,
            contextual_lanes: AlignmentNavigationProductState::Independent,
        }
    }
}

/// One auditable alignment-target state machine. Terminal phases have no owner; every other live
/// phase names exactly one component which must produce the next transition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AlignmentNavigationPhase {
    #[default]
    Dormant,
    Queued,
    Running,
    WaitingForSourcePressure,
    CandidateReady,
    WaitingForGpuPressure,
    BudgetLimited,
    Committed,
    IndexOverview,
    Unavailable,
    Failed,
    Superseded,
}

impl AlignmentNavigationPhase {
    pub(crate) const fn owner(self) -> Option<AlignmentNavigationOwner> {
        match self {
            Self::Queued => Some(AlignmentNavigationOwner::DesktopQueue),
            Self::Running => Some(AlignmentNavigationOwner::SourceWorker),
            Self::WaitingForSourcePressure => Some(AlignmentNavigationOwner::SourcePressure),
            Self::CandidateReady => Some(AlignmentNavigationOwner::GpuCandidate),
            Self::WaitingForGpuPressure => Some(AlignmentNavigationOwner::GpuPressure),
            Self::Dormant
            | Self::BudgetLimited
            | Self::Committed
            | Self::IndexOverview
            | Self::Unavailable
            | Self::Failed
            | Self::Superseded => None,
        }
    }

    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Committed
                | Self::BudgetLimited
                | Self::IndexOverview
                | Self::Unavailable
                | Self::Failed
                | Self::Superseded
        )
    }

    pub(crate) const fn is_settled_current(self) -> bool {
        matches!(
            self,
            Self::Committed
                | Self::IndexOverview
                | Self::Unavailable
                | Self::Failed
                | Self::BudgetLimited
        )
    }

    pub(crate) const fn status(self) -> &'static str {
        match self {
            Self::Dormant => "No alignment target",
            Self::Queued => "Alignment target queued",
            Self::Running => "Loading complete alignment evidence",
            Self::WaitingForSourcePressure => "Waiting for alignment cache capacity",
            Self::CandidateReady => "Preparing alignment presentation",
            Self::WaitingForGpuPressure => "Waiting for GPU cache capacity",
            Self::BudgetLimited => "Alignment presentation exceeds the cache budget",
            Self::Committed => "Alignment presentation committed",
            Self::IndexOverview => "Index-only alignment overview",
            Self::Unavailable => "Alignment target unavailable",
            Self::Failed => "Alignment target failed",
            Self::Superseded => "Alignment target superseded",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AlignmentNavigationJournalEntry {
    pub(crate) panel_id: Option<PanelId>,
    pub(crate) track_id: Option<TrackId>,
    pub(crate) generation: u64,
    pub(crate) request_token: Option<AlignmentSceneRequestToken>,
    pub(crate) camera: Option<AlignmentSceneCamera>,
    pub(crate) from: AlignmentNavigationPhase,
    pub(crate) to: AlignmentNavigationPhase,
    pub(crate) owner: Option<AlignmentNavigationOwner>,
    pub(crate) products: AlignmentNavigationProducts,
    pub(crate) reason: Arc<str>,
}

#[derive(Clone, Debug)]
pub(crate) struct AlignmentNavigationDiagnostic {
    pub(crate) panel_id: Option<PanelId>,
    pub(crate) track_id: Option<TrackId>,
    pub(crate) generation: u64,
    pub(crate) request_token: Option<AlignmentSceneRequestToken>,
    pub(crate) desired_camera: Option<AlignmentSceneCamera>,
    pub(crate) phase: AlignmentNavigationPhase,
    pub(crate) owner: Option<AlignmentNavigationOwner>,
    pub(crate) products: AlignmentNavigationProducts,
    pub(crate) detail: Option<Arc<str>>,
    pub(crate) journal_entries: usize,
    pub(crate) invariant_failure: Option<Arc<str>>,
}

/// Live state and a bounded diagnostic history for one panel/track target. This record does not
/// retain reads, pages, or allocation inventories, so observing it is constant-cost.
#[derive(Clone, Debug)]
pub(crate) struct AlignmentNavigationTransaction {
    panel_id: Option<PanelId>,
    track_id: Option<TrackId>,
    generation: u64,
    request_token: Option<AlignmentSceneRequestToken>,
    desired_camera: Option<AlignmentSceneCamera>,
    phase: AlignmentNavigationPhase,
    products: AlignmentNavigationProducts,
    detail: Option<Arc<str>>,
    journal: VecDeque<AlignmentNavigationJournalEntry>,
}

impl Default for AlignmentNavigationTransaction {
    fn default() -> Self {
        Self {
            panel_id: None,
            track_id: None,
            generation: 0,
            request_token: None,
            desired_camera: None,
            phase: AlignmentNavigationPhase::Dormant,
            products: AlignmentNavigationProducts::default(),
            detail: None,
            journal: VecDeque::with_capacity(ALIGNMENT_NAVIGATION_JOURNAL_CAPACITY),
        }
    }
}

impl AlignmentNavigationTransaction {
    pub(crate) fn bind_identity(&mut self, panel_id: PanelId, track_id: TrackId) {
        self.panel_id = Some(panel_id);
        self.track_id = Some(track_id);
    }

    pub(crate) fn begin(
        &mut self,
        generation: u64,
        request_token: AlignmentSceneRequestToken,
        reason: impl Into<Arc<str>>,
    ) {
        let reason = reason.into();
        if !matches!(self.phase, AlignmentNavigationPhase::Dormant) {
            let previous = self.phase;
            if !previous.is_terminal() {
                self.apply_phase_products(AlignmentNavigationPhase::Superseded);
                self.record_transition(
                    previous,
                    AlignmentNavigationPhase::Superseded,
                    Arc::from("newer alignment target replaced this target"),
                );
                self.phase = AlignmentNavigationPhase::Superseded;
            }
        }
        let previous = self.phase;
        self.generation = generation;
        self.request_token = Some(request_token);
        self.desired_camera = None;
        self.phase = AlignmentNavigationPhase::Queued;
        self.products = AlignmentNavigationProducts {
            source: AlignmentNavigationProductState::Queued,
            row_atlas: AlignmentNavigationProductState::Pending,
            semantic_pages: AlignmentNavigationProductState::Pending,
            gpu: AlignmentNavigationProductState::Pending,
            reference: AlignmentNavigationProductState::Pending,
            contextual_lanes: AlignmentNavigationProductState::Independent,
        };
        self.detail = Some(Arc::clone(&reason));
        self.record_transition(previous, AlignmentNavigationPhase::Queued, reason);
    }

    pub(crate) fn bind_camera(&mut self, camera: AlignmentSceneCamera) {
        self.desired_camera = Some(camera);
    }

    pub(crate) fn transition(
        &mut self,
        next: AlignmentNavigationPhase,
        reason: impl Into<Arc<str>>,
    ) {
        let reason = reason.into();
        if self.phase == next {
            self.detail = Some(reason);
            return;
        }
        let previous = self.phase;
        if !alignment_navigation_transition_is_legal(previous, next) {
            let diagnostic: Arc<str> = Arc::from(format!(
                "illegal alignment navigation transition {previous:?} -> {next:?}: {reason}"
            ));
            self.phase = AlignmentNavigationPhase::Failed;
            self.apply_phase_products(AlignmentNavigationPhase::Failed);
            self.detail = Some(Arc::clone(&diagnostic));
            self.record_transition(previous, AlignmentNavigationPhase::Failed, diagnostic);
            return;
        }
        self.phase = next;
        self.apply_phase_products(next);
        self.detail = Some(Arc::clone(&reason));
        self.record_transition(previous, next, reason);
    }

    /// Records the semantic/reference dependency state carried by an immutable worker frame.
    /// This observation is diagnostic only; the explicit phase transition remains the sole
    /// navigation liveness authority.
    pub(crate) fn observe_frame_products(&mut self, frame: &AlignmentCoordinatorFrame) {
        let mut products = self.products;
        products.source = match frame.individual_reads {
            AlignmentIndividualReadCapability::Pending => AlignmentNavigationProductState::Running,
            AlignmentIndividualReadCapability::Complete { .. } => {
                AlignmentNavigationProductState::Ready
            }
            AlignmentIndividualReadCapability::Refused { .. } => {
                AlignmentNavigationProductState::Unavailable
            }
        };
        products.row_atlas = if !frame.camera.has_visible_rows() {
            AlignmentNavigationProductState::NotRequired
        } else if frame.snapshot.is_some() && frame.row_activity.is_some() {
            AlignmentNavigationProductState::Ready
        } else {
            AlignmentNavigationProductState::Pending
        };
        products.semantic_pages = if frame.visible_complete() {
            AlignmentNavigationProductState::Ready
        } else {
            AlignmentNavigationProductState::Pending
        };
        products.reference = match frame.reference_requirement {
            AlignmentReferenceRequirement::NotRequired => {
                AlignmentNavigationProductState::NotRequired
            }
            AlignmentReferenceRequirement::Pending { .. } => {
                AlignmentNavigationProductState::Pending
            }
            AlignmentReferenceRequirement::Ready { .. } if frame.reference_products_current => {
                AlignmentNavigationProductState::Ready
            }
            AlignmentReferenceRequirement::Ready { .. } => AlignmentNavigationProductState::Pending,
        };
        if products == self.products {
            return;
        }
        self.products = products;
        self.record_transition(
            self.phase,
            self.phase,
            Arc::from("alignment product readiness changed"),
        );
    }

    pub(crate) fn observe_reference_failure(&mut self, reason: impl Into<Arc<str>>) {
        if !matches!(
            self.products.reference,
            AlignmentNavigationProductState::Pending
                | AlignmentNavigationProductState::Queued
                | AlignmentNavigationProductState::Running
                | AlignmentNavigationProductState::Pressure
        ) {
            return;
        }
        self.products.reference = AlignmentNavigationProductState::Failed;
        self.record_transition(self.phase, self.phase, reason.into());
    }

    /// Records contextual annotation truth without transferring alignment liveness ownership.
    /// These states are deliberately independent variants: pending or failed generic lanes cannot
    /// reopen, fail, or otherwise delay the core immutable alignment epoch.
    pub(crate) fn observe_contextual_lanes(
        &mut self,
        state: AlignmentNavigationProductState,
        reason: impl Into<Arc<str>>,
    ) {
        debug_assert!(matches!(
            state,
            AlignmentNavigationProductState::Independent
                | AlignmentNavigationProductState::IndependentPending
                | AlignmentNavigationProductState::IndependentReady
                | AlignmentNavigationProductState::IndependentFailed
        ));
        if self.products.contextual_lanes == state {
            return;
        }
        self.products.contextual_lanes = state;
        self.record_transition(self.phase, self.phase, reason.into());
    }

    #[cfg(test)]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) const fn request_token(&self) -> Option<AlignmentSceneRequestToken> {
        self.request_token
    }

    pub(crate) const fn desired_camera(&self) -> Option<AlignmentSceneCamera> {
        self.desired_camera
    }

    pub(crate) const fn phase(&self) -> AlignmentNavigationPhase {
        self.phase
    }

    pub(crate) const fn is_queued(&self) -> bool {
        matches!(self.phase, AlignmentNavigationPhase::Queued)
    }

    pub(crate) const fn is_current_settled(&self, generation: u64) -> bool {
        self.generation == generation && self.phase.is_settled_current()
    }

    pub(crate) fn event_is_current(
        &self,
        request_token: AlignmentSceneRequestToken,
        camera: AlignmentSceneCamera,
    ) -> bool {
        self.request_token == Some(request_token) && self.desired_camera == Some(camera)
    }

    pub(crate) fn fail_if_source_owner_is_idle(
        &mut self,
        source_pending: bool,
        presentation_pending: bool,
    ) -> bool {
        if self.phase != AlignmentNavigationPhase::Running || source_pending || presentation_pending
        {
            return false;
        }
        self.transition(
            AlignmentNavigationPhase::Failed,
            "alignment target is unsettled but the source worker owns no work",
        );
        true
    }

    pub(crate) fn diagnostic(&self) -> AlignmentNavigationDiagnostic {
        let owner = self.phase.owner();
        let invariant_failure = (!self.phase.is_terminal()
            && self.phase != AlignmentNavigationPhase::Dormant
            && owner.is_none())
        .then(|| Arc::from("unsettled alignment target has no current owner"));
        AlignmentNavigationDiagnostic {
            panel_id: self.panel_id,
            track_id: self.track_id,
            generation: self.generation,
            request_token: self.request_token,
            desired_camera: self.desired_camera,
            phase: self.phase,
            owner,
            products: self.products,
            detail: self.detail.as_ref().map(Arc::clone),
            journal_entries: self.journal.len(),
            invariant_failure,
        }
    }

    pub(crate) fn journal(
        &self,
    ) -> impl DoubleEndedIterator<Item = &AlignmentNavigationJournalEntry> {
        self.journal.iter()
    }

    fn record_transition(
        &mut self,
        from: AlignmentNavigationPhase,
        to: AlignmentNavigationPhase,
        reason: Arc<str>,
    ) {
        if self.journal.len() == ALIGNMENT_NAVIGATION_JOURNAL_CAPACITY {
            self.journal.pop_front();
        }
        self.journal.push_back(AlignmentNavigationJournalEntry {
            panel_id: self.panel_id,
            track_id: self.track_id,
            generation: self.generation,
            request_token: self.request_token,
            camera: self.desired_camera,
            from,
            to,
            owner: to.owner(),
            products: self.products,
            reason,
        });
    }

    fn apply_phase_products(&mut self, phase: AlignmentNavigationPhase) {
        use AlignmentNavigationProductState as Product;
        match phase {
            AlignmentNavigationPhase::Dormant => {
                self.products = AlignmentNavigationProducts::default();
            }
            AlignmentNavigationPhase::Queued => {
                self.products.source = Product::Queued;
                self.products.row_atlas = Product::Pending;
                self.products.semantic_pages = Product::Pending;
                self.products.gpu = Product::Pending;
            }
            AlignmentNavigationPhase::Running => {
                self.products.source = Product::Running;
                self.products.gpu = Product::Pending;
            }
            AlignmentNavigationPhase::WaitingForSourcePressure => {
                self.products.source = Product::Pressure;
            }
            AlignmentNavigationPhase::CandidateReady => {
                self.products.source = Product::Ready;
                if self.products.row_atlas != Product::NotRequired {
                    self.products.row_atlas = Product::Ready;
                }
                self.products.semantic_pages = Product::Ready;
                self.products.gpu = Product::Queued;
            }
            AlignmentNavigationPhase::WaitingForGpuPressure => {
                self.products.gpu = Product::Pressure;
            }
            AlignmentNavigationPhase::BudgetLimited => {
                self.products.source = Product::Ready;
                if self.products.row_atlas != Product::NotRequired {
                    self.products.row_atlas = Product::Ready;
                }
                self.products.semantic_pages = Product::Ready;
                self.products.gpu = Product::BudgetLimited;
            }
            AlignmentNavigationPhase::Committed => {
                self.products.source = Product::Ready;
                if self.products.row_atlas != Product::NotRequired {
                    self.products.row_atlas = Product::Ready;
                }
                self.products.semantic_pages = Product::Ready;
                self.products.gpu = Product::Ready;
            }
            AlignmentNavigationPhase::IndexOverview => {
                self.products.source = Product::Ready;
                self.products.row_atlas = Product::NotRequired;
                self.products.semantic_pages = Product::NotRequired;
                self.products.gpu = Product::NotRequired;
            }
            AlignmentNavigationPhase::Unavailable => {
                self.products.source = Product::Unavailable;
                self.products.row_atlas = Product::Unavailable;
                self.products.semantic_pages = Product::Unavailable;
                self.products.gpu = Product::NotRequired;
            }
            AlignmentNavigationPhase::Failed => {
                self.products.source = Product::Failed;
                self.products.row_atlas = Product::Failed;
                self.products.semantic_pages = Product::Failed;
                self.products.gpu = Product::Failed;
            }
            AlignmentNavigationPhase::Superseded => {
                self.products.source = Product::Superseded;
                self.products.row_atlas = Product::Superseded;
                self.products.semantic_pages = Product::Superseded;
                self.products.gpu = Product::Superseded;
            }
        }
    }
}

const fn alignment_navigation_transition_is_legal(
    from: AlignmentNavigationPhase,
    to: AlignmentNavigationPhase,
) -> bool {
    use AlignmentNavigationPhase as Phase;
    match from {
        Phase::Dormant => matches!(
            to,
            Phase::Queued | Phase::IndexOverview | Phase::Unavailable | Phase::Failed
        ),
        Phase::Queued => matches!(
            to,
            Phase::Running
                | Phase::IndexOverview
                | Phase::Unavailable
                | Phase::Failed
                | Phase::Superseded
        ),
        Phase::Running => matches!(
            to,
            Phase::WaitingForSourcePressure
                | Phase::CandidateReady
                | Phase::Unavailable
                | Phase::Failed
                | Phase::Superseded
        ),
        Phase::WaitingForSourcePressure => matches!(
            to,
            Phase::Running | Phase::Unavailable | Phase::Failed | Phase::Superseded
        ),
        Phase::CandidateReady => matches!(
            to,
            Phase::Committed
                | Phase::WaitingForGpuPressure
                | Phase::BudgetLimited
                | Phase::Failed
                | Phase::Superseded
        ),
        Phase::WaitingForGpuPressure => matches!(
            to,
            Phase::CandidateReady
                | Phase::Committed
                | Phase::BudgetLimited
                | Phase::Failed
                | Phase::Superseded
        ),
        Phase::Committed
        | Phase::BudgetLimited
        | Phase::IndexOverview
        | Phase::Unavailable
        | Phase::Failed
        | Phase::Superseded => matches!(to, Phase::Queued),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::{ContigId, GenomicInterval, Viewport};

    fn camera_on(contig: u32, start: u64) -> AlignmentSceneCamera {
        AlignmentSceneCamera::without_visible_rows(
            Viewport::new(
                GenomicInterval::new(ContigId::new(contig), start, start + 100).unwrap(),
                800.0,
            )
            .unwrap(),
        )
    }

    fn camera(start: u64) -> AlignmentSceneCamera {
        camera_on(0, start)
    }

    #[test]
    fn every_live_phase_names_exactly_one_owner() {
        for phase in [
            AlignmentNavigationPhase::Queued,
            AlignmentNavigationPhase::Running,
            AlignmentNavigationPhase::WaitingForSourcePressure,
            AlignmentNavigationPhase::CandidateReady,
            AlignmentNavigationPhase::WaitingForGpuPressure,
        ] {
            assert!(phase.owner().is_some(), "{phase:?}");
            assert!(!phase.is_terminal());
        }
    }

    #[test]
    fn superseding_records_the_displaced_target_and_keeps_exact_latest_intent() {
        let mut transaction = AlignmentNavigationTransaction::default();
        transaction.begin(1, AlignmentSceneRequestToken::new(10), "first target");
        transaction.bind_camera(camera(100));
        transaction.transition(AlignmentNavigationPhase::Running, "source accepted target");
        transaction.begin(2, AlignmentSceneRequestToken::new(11), "latest target");
        transaction.bind_camera(camera(200));

        assert_eq!(transaction.generation(), 2);
        assert_eq!(
            transaction.request_token(),
            Some(AlignmentSceneRequestToken::new(11))
        );
        assert_eq!(transaction.desired_camera(), Some(camera(200)));
        assert_eq!(transaction.phase(), AlignmentNavigationPhase::Queued);
        assert!(transaction.journal().any(|entry| {
            entry.generation == 1 && entry.to == AlignmentNavigationPhase::Superseded
        }));
    }

    #[test]
    fn idle_source_cannot_leave_a_running_target_unsettled() {
        let mut transaction = AlignmentNavigationTransaction::default();
        transaction.begin(1, AlignmentSceneRequestToken::new(10), "target");
        transaction.bind_camera(camera(100));
        transaction.transition(AlignmentNavigationPhase::Running, "queued at source");

        assert!(transaction.fail_if_source_owner_is_idle(false, false));
        assert_eq!(transaction.phase(), AlignmentNavigationPhase::Failed);
        assert!(transaction.is_current_settled(1));
    }

    #[test]
    fn journal_is_bounded_under_an_arbitrary_input_burst() {
        let mut transaction = AlignmentNavigationTransaction::default();
        for generation in 1..=1_000 {
            transaction.begin(
                generation,
                AlignmentSceneRequestToken::new(generation),
                "burst target",
            );
            transaction.bind_camera(camera(generation));
            transaction.transition(AlignmentNavigationPhase::Running, "source accepted target");
        }
        assert_eq!(
            transaction.journal().count(),
            ALIGNMENT_NAVIGATION_JOURNAL_CAPACITY
        );
        assert_eq!(transaction.generation(), 1_000);
    }

    #[test]
    fn rapid_pan_zoom_reversal_commits_only_the_exact_latest_target() {
        let mut transaction = AlignmentNavigationTransaction::default();
        let mut stale = Vec::new();
        for generation in 1..=10_000_u64 {
            let token = AlignmentSceneRequestToken::new(generation.saturating_mul(17));
            let start = if generation % 2 == 0 {
                generation.saturating_mul(101)
            } else {
                2_000_000_u64.saturating_sub(generation.saturating_mul(97))
            };
            if let (Some(token), Some(camera)) =
                (transaction.request_token(), transaction.desired_camera())
            {
                stale.push((token, camera));
                if stale.len() > 8 {
                    stale.remove(0);
                }
            }
            transaction.begin(generation, token, "coalesced navigation input");
            transaction.bind_camera(camera(start));
            transaction.transition(AlignmentNavigationPhase::Running, "latest source work");
        }

        let latest_token = AlignmentSceneRequestToken::new(10_000 * 17);
        let latest_camera = camera(10_000 * 101);
        assert!(transaction.event_is_current(latest_token, latest_camera));
        assert!(
            stale
                .into_iter()
                .all(|(token, camera)| !transaction.event_is_current(token, camera))
        );
        transaction.transition(
            AlignmentNavigationPhase::CandidateReady,
            "latest semantic candidate",
        );
        transaction.transition(
            AlignmentNavigationPhase::WaitingForGpuPressure,
            "typed GPU pressure",
        );
        transaction.transition(
            AlignmentNavigationPhase::CandidateReady,
            "GPU progress notification",
        );
        transaction.transition(
            AlignmentNavigationPhase::Committed,
            "atomic latest publication",
        );

        assert!(transaction.is_current_settled(10_000));
        assert_eq!(transaction.request_token(), Some(latest_token));
        assert_eq!(transaction.desired_camera(), Some(latest_camera));
        assert_eq!(transaction.diagnostic().owner, None);
        assert_eq!(transaction.diagnostic().invariant_failure, None);
        assert_eq!(
            transaction.journal().count(),
            ALIGNMENT_NAVIGATION_JOURNAL_CAPACITY
        );
    }

    #[test]
    fn exhausted_gpu_capacity_settles_without_becoming_source_unavailable() {
        let mut transaction = AlignmentNavigationTransaction::default();
        transaction.begin(1, AlignmentSceneRequestToken::new(10), "target");
        transaction.bind_camera(camera(100));
        transaction.transition(AlignmentNavigationPhase::Running, "source accepted target");
        transaction.transition(
            AlignmentNavigationPhase::CandidateReady,
            "semantic candidate ready",
        );
        transaction.transition(
            AlignmentNavigationPhase::WaitingForGpuPressure,
            "aggregate reclaim",
        );
        transaction.transition(
            AlignmentNavigationPhase::BudgetLimited,
            "protected minimum cannot fit",
        );

        assert!(transaction.is_current_settled(1));
        assert_eq!(transaction.phase(), AlignmentNavigationPhase::BudgetLimited);
        assert_eq!(
            transaction.diagnostic().products.source,
            AlignmentNavigationProductState::Ready
        );
        assert_eq!(
            transaction.diagnostic().products.gpu,
            AlignmentNavigationProductState::BudgetLimited
        );
        assert_eq!(transaction.diagnostic().owner, None);
    }

    #[test]
    fn repeated_contig_changes_reject_every_stale_completion_and_settle_the_latest_target() {
        let mut transaction = AlignmentNavigationTransaction::default();
        let mut displaced = Vec::new();
        for generation in 1..=4_096_u64 {
            if let (Some(token), Some(camera)) =
                (transaction.request_token(), transaction.desired_camera())
            {
                displaced.push((token, camera));
                if displaced.len() > 16 {
                    displaced.remove(0);
                }
            }
            let token = AlignmentSceneRequestToken::new(generation.saturating_mul(29));
            let target = camera_on((generation % 4) as u32, generation.saturating_mul(113));
            transaction.begin(generation, token, "rapid contig navigation");
            transaction.bind_camera(target);
            transaction.transition(AlignmentNavigationPhase::Running, "latest source work");
        }

        let latest_token = AlignmentSceneRequestToken::new(4_096 * 29);
        let latest_camera = camera_on(0, 4_096 * 113);
        assert!(transaction.event_is_current(latest_token, latest_camera));
        assert!(
            displaced
                .into_iter()
                .all(|(token, camera)| !transaction.event_is_current(token, camera))
        );
        transaction.transition(
            AlignmentNavigationPhase::CandidateReady,
            "latest scene ready",
        );
        transaction.transition(AlignmentNavigationPhase::Committed, "atomic publication");

        let diagnostic = transaction.diagnostic();
        assert!(transaction.is_current_settled(4_096));
        assert_eq!(diagnostic.owner, None);
        assert_eq!(diagnostic.invariant_failure, None);
        assert_eq!(
            transaction.journal().count(),
            ALIGNMENT_NAVIGATION_JOURNAL_CAPACITY
        );
    }

    #[test]
    fn source_pressure_remains_owned_and_resumes_without_changing_identity() {
        let mut transaction = AlignmentNavigationTransaction::default();
        let token = AlignmentSceneRequestToken::new(91);
        let target = camera(4_000);
        transaction.begin(12, token, "target");
        transaction.bind_camera(target);
        transaction.transition(AlignmentNavigationPhase::Running, "source accepted");
        transaction.transition(
            AlignmentNavigationPhase::WaitingForSourcePressure,
            "typed ledger obstruction",
        );
        assert_eq!(
            transaction.diagnostic().products.source,
            AlignmentNavigationProductState::Pressure
        );
        assert_eq!(
            transaction.diagnostic().owner,
            Some(AlignmentNavigationOwner::SourcePressure)
        );
        transaction.transition(AlignmentNavigationPhase::Running, "capacity progressed");
        transaction.transition(AlignmentNavigationPhase::CandidateReady, "scene ready");
        transaction.transition(AlignmentNavigationPhase::Committed, "published");
        assert_eq!(transaction.request_token(), Some(token));
        assert_eq!(transaction.desired_camera(), Some(target));
        assert!(transaction.is_current_settled(12));
        assert_eq!(
            transaction.diagnostic().products.gpu,
            AlignmentNavigationProductState::Ready
        );
    }

    #[test]
    fn cache_clear_reissues_a_committed_camera_and_rejects_the_pre_clear_token() {
        let mut transaction = AlignmentNavigationTransaction::default();
        transaction.bind_identity(PanelId::new(4), TrackId::new(9));
        let target = camera(8_000);
        let old_token = AlignmentSceneRequestToken::new(201);
        transaction.begin(21, old_token, "initial target");
        transaction.bind_camera(target);
        transaction.transition(AlignmentNavigationPhase::Running, "source accepted");
        transaction.transition(AlignmentNavigationPhase::CandidateReady, "scene ready");
        transaction.transition(AlignmentNavigationPhase::Committed, "published");

        let refresh_token = AlignmentSceneRequestToken::new(202);
        transaction.begin(21, refresh_token, "cache cleared; reissue current target");
        transaction.bind_camera(target);
        assert!(!transaction.event_is_current(old_token, target));
        assert!(transaction.event_is_current(refresh_token, target));
        assert_eq!(transaction.phase(), AlignmentNavigationPhase::Queued);

        transaction.transition(
            AlignmentNavigationPhase::Running,
            "post-clear source accepted",
        );
        transaction.transition(
            AlignmentNavigationPhase::CandidateReady,
            "post-clear scene ready",
        );
        transaction.transition(
            AlignmentNavigationPhase::Committed,
            "post-clear GPU published",
        );
        assert!(transaction.is_current_settled(21));
        assert_eq!(transaction.request_token(), Some(refresh_token));
        assert_eq!(transaction.diagnostic().invariant_failure, None);
        let latest = transaction.journal().next_back().unwrap();
        assert_eq!(latest.panel_id, Some(PanelId::new(4)));
        assert_eq!(latest.track_id, Some(TrackId::new(9)));
    }

    #[test]
    fn optional_reference_failure_is_journaled_without_reopening_a_committed_target() {
        let mut transaction = AlignmentNavigationTransaction::default();
        let target = camera(900);
        transaction.begin(4, AlignmentSceneRequestToken::new(44), "target");
        transaction.bind_camera(target);
        transaction.transition(AlignmentNavigationPhase::Running, "source accepted");
        transaction.transition(AlignmentNavigationPhase::CandidateReady, "core scene ready");
        transaction.transition(AlignmentNavigationPhase::Committed, "core scene published");

        transaction.observe_reference_failure("optional FASTA query failed");
        let diagnostic = transaction.diagnostic();
        assert_eq!(diagnostic.phase, AlignmentNavigationPhase::Committed);
        assert_eq!(
            diagnostic.products.reference,
            AlignmentNavigationProductState::Failed
        );
        assert!(transaction.is_current_settled(4));
        assert!(
            transaction
                .journal()
                .next_back()
                .is_some_and(|entry| entry.from == AlignmentNavigationPhase::Committed
                    && entry.to == AlignmentNavigationPhase::Committed)
        );
    }

    #[test]
    fn contextual_annotation_pending_ready_and_failure_never_own_core_navigation() {
        let mut transaction = AlignmentNavigationTransaction::default();
        transaction.begin(8, AlignmentSceneRequestToken::new(81), "target");
        transaction.bind_camera(camera(1_200));
        transaction.transition(AlignmentNavigationPhase::Running, "source accepted");
        transaction.observe_contextual_lanes(
            AlignmentNavigationProductState::IndependentPending,
            "contextual annotation remains pending",
        );
        transaction.transition(AlignmentNavigationPhase::CandidateReady, "core scene ready");
        transaction.transition(AlignmentNavigationPhase::Committed, "core scene published");
        assert!(transaction.is_current_settled(8));
        assert_eq!(transaction.diagnostic().owner, None);

        transaction.observe_contextual_lanes(
            AlignmentNavigationProductState::IndependentReady,
            "contextual annotation published independently",
        );
        assert_eq!(transaction.phase(), AlignmentNavigationPhase::Committed);
        transaction.observe_contextual_lanes(
            AlignmentNavigationProductState::IndependentFailed,
            "contextual annotation failed independently",
        );
        let diagnostic = transaction.diagnostic();
        assert_eq!(diagnostic.phase, AlignmentNavigationPhase::Committed);
        assert_eq!(diagnostic.owner, None);
        assert_eq!(
            diagnostic.products.contextual_lanes,
            AlignmentNavigationProductState::IndependentFailed
        );
        assert!(transaction.is_current_settled(8));
    }

    #[test]
    fn illegal_or_lost_completion_fails_instead_of_spinning() {
        let mut transaction = AlignmentNavigationTransaction::default();
        transaction.begin(3, AlignmentSceneRequestToken::new(33), "target");
        transaction.bind_camera(camera(300));
        transaction.transition(
            AlignmentNavigationPhase::Committed,
            "publication skipped every required owner",
        );
        let diagnostic = transaction.diagnostic();
        assert_eq!(diagnostic.phase, AlignmentNavigationPhase::Failed);
        assert!(
            diagnostic
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("illegal alignment navigation transition"))
        );
        assert!(transaction.is_current_settled(3));
    }
}
