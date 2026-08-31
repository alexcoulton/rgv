use super::super::*;

/// Canonical presentation partition used by the first desktop qualification slice. This is a
/// cache/page granularity, never an evidence or display cap. Measurements may replace it without
/// changing scene identity or completeness semantics.
const DESKTOP_ALIGNMENT_SCENE_TILE_BASES: u64 = 32_768;
const DESKTOP_ALIGNMENT_SCENE_ROW_BLOCK_ROWS: u32 = 256;

/// Panel-local coherence identities. Raw and CPM scales are deliberately different groups: a
/// numerical maximum in one unit can never be applied to the other.
pub(crate) const ALIGNMENT_SASHIMI_RAW_GROUP: AlignmentCoherenceGroupId =
    AlignmentCoherenceGroupId::new(1);
pub(crate) const ALIGNMENT_SASHIMI_CPM_GROUP: AlignmentCoherenceGroupId =
    AlignmentCoherenceGroupId::new(2);

pub(crate) const fn alignment_scene_sashimi_group(
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
pub(crate) fn alignment_scene_selected_position(
    selected_track: Option<TrackId>,
    track_id: TrackId,
    position: Option<u64>,
) -> Option<u64> {
    (selected_track == Some(track_id))
        .then_some(position)
        .flatten()
}

pub(crate) fn expanded_workspace_alignment_limit(
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
pub(crate) fn alignment_scene_coverage_lane_options(
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
pub(crate) fn alignment_scene_sashimi_lane_options(
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

pub(crate) fn alignment_scene_modification_lane_options(
    filters: &SessionFilters,
    selected_position: Option<u64>,
) -> AlignmentModificationLaneOptions {
    let mut hidden_codes = filters
        .hidden_modification_codes
        .iter()
        .filter_map(|value| crate::ui::alignment_settings::parse_modification_code(value))
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

/// Resolves scene-owned derived lanes from the already-final pane layout. The lane's stable
/// `AlignmentSubtrack` identity—not its user-facing label—selects semantics, and the returned clip
/// is intersected with the final scrolled item clip used by gutter hit testing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn alignment_scene_derived_lane_captures(
    layout: &ui::PaneLayoutSnapshot,
    track_id: TrackId,
    horizontal_x: f64,
    horizontal_width: f64,
    filters: &SessionFilters,
    reference_backed_alleles: bool,
    sashimi_group: Option<AlignmentCoherenceGroupId>,
    selected_pileup_position: Option<u64>,
    selected_modification_position: Option<u64>,
) -> Result<Vec<AlignmentDerivedLaneCapture>, AlignmentPanelSceneError> {
    let Some(item) = layout.items.iter().find(|item| {
        item.frame.key == ui::TrackFrameKey::Alignment(track_id)
            && item.frame.channel == ui::TrackPane::Annotation
    }) else {
        return Ok(Vec::new());
    };
    if !item.effective_content_visible || item.clip_rect.height() <= 0.0 {
        return Ok(Vec::new());
    }
    let mut captures = Vec::new();
    for subtrack in &item.frame.subtracks {
        if subtrack.collapsed {
            continue;
        }
        let Some(kind) = subtrack.alignment_subtrack else {
            continue;
        };
        let top = (subtrack.top - layout.scroll_points).max(item.clip_rect.top());
        let bottom = (subtrack.bottom - layout.scroll_points).min(item.clip_rect.bottom());
        if bottom <= top {
            continue;
        }
        let options = match kind {
            ui::AlignmentSubtrack::Coverage if filters.show_coverage => {
                AlignmentDerivedLaneOptions::Coverage(alignment_scene_coverage_lane_options(
                    filters,
                    reference_backed_alleles,
                    selected_pileup_position,
                ))
            }
            ui::AlignmentSubtrack::Sashimi if filters.show_junctions => {
                let Some(options) = alignment_scene_sashimi_lane_options(filters, sashimi_group)
                else {
                    // A Shared lane cannot truthfully enter an epoch without its explicit group.
                    continue;
                };
                AlignmentDerivedLaneOptions::Sashimi(options)
            }
            ui::AlignmentSubtrack::Modifications if filters.show_modification_frequency => {
                AlignmentDerivedLaneOptions::Modifications(
                    alignment_scene_modification_lane_options(
                        filters,
                        selected_modification_position,
                    ),
                )
            }
            ui::AlignmentSubtrack::Reference
            | ui::AlignmentSubtrack::Reads
            | ui::AlignmentSubtrack::Coverage
            | ui::AlignmentSubtrack::Sashimi
            | ui::AlignmentSubtrack::Modifications => continue,
        };
        captures.push(AlignmentDerivedLaneCapture::new(
            CpuClipRect {
                x: horizontal_x,
                y: f64::from(top),
                width: horizontal_width,
                height: f64::from(bottom - top),
            },
            options,
        )?);
    }
    Ok(captures)
}

/// Actual read-pixel clip from the canonical Reads pane. A hidden lane returns `None`; callers
/// must not manufacture a read render camera merely to keep summary acquisition alive.
pub(crate) fn alignment_scene_read_clip(
    layout: &ui::PaneLayoutSnapshot,
    track_id: TrackId,
    horizontal_x: f64,
    horizontal_width: f64,
) -> Option<CpuClipRect> {
    let item = layout.items.iter().find(|item| {
        item.frame.key == ui::TrackFrameKey::Alignment(track_id)
            && item.frame.channel == ui::TrackPane::Reads
    })?;
    let reads_visible = item.effective_content_visible
        && item.frame.subtracks.iter().any(|subtrack| {
            subtrack.alignment_subtrack == Some(ui::AlignmentSubtrack::Reads) && !subtrack.collapsed
        });
    let content = reads_visible.then_some(item.content_rect).flatten()?;
    (content.height() > 0.0).then_some(CpuClipRect {
        x: horizontal_x,
        y: f64::from(content.top()),
        width: horizontal_width,
        height: f64::from(content.height()),
    })
}

#[derive(Clone)]
pub(crate) struct FilterEditor {
    pub(crate) minimum_mapq: u8,
    pub(crate) include_secondary: bool,
    pub(crate) include_supplementary: bool,
    pub(crate) expression: String,
    pub(crate) error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ActiveFilters {
    pub(crate) alignment: AlignmentFilter,
    pub(crate) include_secondary: bool,
    pub(crate) include_supplementary: bool,
}

/// Canonical alignment state used only while no alignment controller exists.
/// Once a source is open, the first controller owns the same state directly.
pub(crate) struct EmptyAlignmentState {
    pub(crate) filters: SessionFilters,
    pub(crate) filter_editor: FilterEditor,
    pub(crate) active_filters: ActiveFilters,
    pub(crate) name: String,
    pub(crate) visible: bool,
    pub(crate) collapsed: bool,
    pub(crate) maximum_alignments: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AlignmentTrackLifecycle {
    Opening,
    Ready,
    Failed(String),
}

/// Whether the exact retained/transient obstruction reported by a ledger error is still present.
/// The progress epoch is deliberately not sufficient: unrelated pins, allocations, or another
/// policy lane can change that epoch without making this request admissible.
pub(crate) trait AlignmentLedgerProgressView {
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

pub(crate) fn alignment_ledger_pressure_blocker_remains(
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
pub(crate) struct AlignmentScenePressureRetry {
    pub(crate) request_token: AlignmentSceneRequestToken,
    pub(crate) target: GenomicInterval,
    pub(crate) pressure: LedgerError,
    pub(crate) ledger_epoch: LedgerProgressEpoch,
    /// True after the exact resume entered the engine mailbox and until either a current frame or
    /// another typed pressure event answers it.
    pub(crate) in_flight: bool,
}

impl AlignmentScenePressureRetry {
    pub(crate) fn same_request(
        self,
        request_token: AlignmentSceneRequestToken,
        target: GenomicInterval,
    ) -> bool {
        self.request_token == request_token && self.target == target
    }

    pub(crate) fn poll(
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
pub(crate) enum AlignmentScenePressureRetryPoll {
    Waiting(AlignmentScenePressureRetry),
    Ready,
}

/// Classifies the first post-relief snapshot without manufacturing an engine retry. Callers store
/// the returned waiting authority verbatim; `Ready` alone permits one mailbox submission.
pub(crate) fn alignment_scene_pressure_after_relief(
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
pub(crate) struct AlignmentSceneBudgetRetry {
    pub(crate) requested_budget: usize,
    pub(crate) clear_before_apply: bool,
    pub(crate) observed_policy_revision: u64,
    pub(crate) observed_budget: usize,
    pub(crate) pressure: LedgerError,
    pub(crate) ledger_epoch: LedgerProgressEpoch,
    /// The first UI-side snapshot proved that the exact worker-reported blocker had already
    /// cleared. This closes the settle-between-event-and-consumption wakeup race without opening
    /// a frame-rate retry loop.
    pub(crate) ready_after_observed_progress: bool,
    pub(crate) in_flight: bool,
}

impl AlignmentSceneBudgetRetry {
    pub(crate) fn after_pressure_event(
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

    pub(crate) fn poll(
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
pub(crate) enum AlignmentSceneBudgetRetryPoll {
    Waiting(AlignmentSceneBudgetRetry),
    Ready,
}

#[cfg(test)]
mod alignment_scene_pressure_retry_tests {
    use super::*;

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

#[derive(Default)]
pub(crate) struct AlignmentPanelProjection {
    pub(crate) clock: GenerationClock,
    pub(crate) unavailable: Option<String>,
    /// The viewport is at overview detail and is intentionally represented by
    /// the alignment index rather than a BAM/CRAM record query.
    pub(crate) index_only_overview: bool,
    /// Generation and start time of the in-flight exact viewport check. Its
    /// visible notice is delayed so fast queries do not flash a banner during
    /// ordinary panning.
    pub(crate) checking_started: Option<(u64, Instant)>,
    pub(crate) checking_notice_visible: bool,
    pub(crate) last_scene_resolution: Option<AlignmentSceneResolution>,
    /// Unified-scene lane identity and latest immutable worker publication. The worker camera is
    /// acquisition intent only; rendering captures this snapshot against the UI's current camera.
    pub(crate) scene_owner: Option<SourceOwnerId>,
    pub(crate) scene_contig: Option<ContigId>,
    pub(crate) scene_contract: Option<AlignmentSceneBuildContract>,
    pub(crate) scene_policy: Option<AlignmentScenePreparationPolicy>,
    /// Sole liveness authority for the latest unified-scene target. No query-local state can
    /// settle or publish this record.
    pub(crate) navigation: AlignmentNavigationTransaction,
    /// Last exact pressure resume admitted for this request at a specific shared-ledger epoch.
    /// It is scheduling state only and never authorizes scene publication.
    pub(crate) scene_pressure_retry: Option<AlignmentScenePressureRetry>,
    /// Last optional-preparation foreground state accepted by the source worker mailbox.
    pub(crate) scene_optional_work_paused: bool,
    pub(crate) scene_frame: Option<AlignmentCoordinatorFrame>,
    pub(crate) scene_frame_request_token: Option<AlignmentSceneRequestToken>,
    pub(crate) scene_telemetry: AlignmentSceneCoordinatorTelemetry,
    pub(crate) scene_interaction: AlignmentInteractionState,
}

impl AlignmentPanelProjection {
    pub(crate) fn settle_failed(&mut self, message: impl Into<String>) {
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

    pub(crate) fn settle_unavailable(&mut self, message: String) {
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
    pub(crate) fn advance_scene_generation(&self) -> ViewportGeneration {
        self.clock.advance()
    }

    /// Whether the projection belongs to the track clock's current generation.
    /// `pending_query` covers work waiting to be dispatched; the generation
    /// comparison also covers a request that has been queued and is still
    /// running. A previous query must not be reprojected or exposed through
    /// interaction while either condition is true.
    pub(crate) fn scene_is_current(&self) -> bool {
        self.navigation
            .is_current_settled(self.clock.current().get())
    }

    pub(crate) fn scene_is_checking(&self) -> bool {
        !self.scene_is_current()
    }

    pub(crate) fn begin_checking(&mut self, now: Instant) {
        let generation = self.clock.current().get();
        if self.checking_started.map(|(seen, _)| seen) != Some(generation) {
            self.checking_started = Some((generation, now));
            self.checking_notice_visible = false;
        }
    }

    pub(crate) fn reveal_checking_notice_if_due(&mut self, now: Instant) -> bool {
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

    pub(crate) fn finish_checking(&mut self) {
        self.checking_started = None;
        self.checking_notice_visible = false;
    }

    pub(crate) fn settle_index_only_overview(&mut self, resolution: AlignmentSceneResolution) {
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

pub(crate) const READ_CHECKING_NOTICE_DELAY: Duration = Duration::from_millis(150);
const RESIZE_COVERAGE_BIN_STEP: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AlignmentSceneResolution {
    pub(crate) detail: DetailLevel,
    pub(crate) coverage_bins: usize,
    pub(crate) collect_alleles: bool,
}

impl AlignmentSceneResolution {
    pub(crate) fn for_viewport(viewport: Viewport) -> Self {
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

    pub(crate) fn requires_refresh_from(self, previous: Self) -> bool {
        self.detail != previous.detail
            || self.collect_alleles != previous.collect_alleles
            || self.coverage_bucket() != previous.coverage_bucket()
    }

    pub(crate) const fn requires_alignment_records(self) -> bool {
        !matches!(self.detail, DetailLevel::Overview)
    }

    const fn coverage_bucket(self) -> usize {
        self.coverage_bins / RESIZE_COVERAGE_BIN_STEP
    }
}

pub(crate) struct AlignmentTrackController {
    pub(crate) config: SessionAlignmentTrack,
    /// New source-scoped immutable-scene authority.
    pub(crate) scene_engine: Option<AlignmentSceneEngine<LocalBamCanonicalBackend>>,
    pub(crate) scene_metadata: Option<AlignmentSceneSourceMetadata>,
    /// Last Complete-read admission policy submitted to the source actor. This avoids flooding
    /// the bounded control mailbox while still making the user setting live and retryable.
    pub(crate) scene_complete_read_limit: usize,
    pub(crate) lifecycle: AlignmentTrackLifecycle,
    pub(crate) dictionary: Option<ReferenceDictionary>,
    pub(crate) index_overview: Option<Arc<rgv_hts::AlignmentIndexOverview>>,
    pub(crate) filter_editor: FilterEditor,
    pub(crate) active_filters: ActiveFilters,
    pub(crate) projections: HashMap<PanelId, AlignmentPanelProjection>,
    /// Last shared scene-ledger limit submitted to the engine actor. Keeping the applied value
    /// here prevents a frame-by-frame update from flooding
    /// the bounded actor queue while its statistics event is in flight.
    pub(crate) scene_budget_bytes: NonZeroUsize,
    /// Typed, progress-gated authority proving that the latest queued budget edit still needs a
    /// coordinator-local retry. This is deliberately separate from `scene_budget_bytes`, which
    /// records submission and cannot prove worker completion.
    pub(crate) scene_budget_retry: Option<AlignmentSceneBudgetRetry>,
}

pub(crate) struct AlignmentSettingsDraft {
    pub(crate) filters: SessionFilters,
    pub(crate) filter_editor: FilterEditor,
    pub(crate) maximum_alignments: usize,
    pub(crate) apply_filter: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AlignmentSettingsOutcome {
    pub(crate) configuration_changed: bool,
    pub(crate) query_changed: bool,
}

impl AlignmentTrackController {
    pub(crate) fn mark_unavailable(&mut self, panel_id: PanelId, message: String) {
        let projection = self.projections.entry(panel_id).or_default();
        projection.settle_unavailable(message);
    }

    pub(crate) fn projection(&self, panel_id: PanelId) -> Option<&AlignmentPanelProjection> {
        self.projections.get(&panel_id)
    }

    pub(crate) fn projection_mut(&mut self, panel_id: PanelId) -> &mut AlignmentPanelProjection {
        self.projections.entry(panel_id).or_default()
    }
}

impl Viewer {
    pub(crate) fn primary_alignment_filters(&self) -> &SessionFilters {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.filters, |track| &track.config.filters)
    }

    pub(crate) fn primary_alignment_filters_mut(&mut self) -> &mut SessionFilters {
        self.alignment_tracks
            .first_mut()
            .map_or(&mut self.empty_alignment.filters, |track| {
                &mut track.config.filters
            })
    }

    pub(crate) fn set_alignment_density_for_track(
        &mut self,
        track_id: TrackId,
        density: f32,
    ) -> bool {
        let density = density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
        let filters =
            if track_id == SYNTHETIC_ALIGNMENT_TRACK_ID && self.alignment_tracks.is_empty() {
                &mut self.empty_alignment.filters
            } else if let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == track_id)
            {
                &mut track.config.filters
            } else {
                return false;
            };
        let changed = (filters.alignment_density - density).abs() > f32::EPSILON;
        filters.alignment_density = density;
        for panel in &mut self.panels.panels {
            if let Some(state) = panel.read_panel_scrolls.get_mut(&track_id) {
                state.density = None;
            }
        }
        if changed {
            self.invalidate_navigation_contract("read density changed");
        }
        changed
    }

    pub(crate) fn alignment_density_for_panel_track(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> f32 {
        self.panels
            .get(panel_id)
            .and_then(|panel| panel.read_panel_scrolls.get(&track_id))
            .and_then(|state| state.density)
            .unwrap_or_else(|| self.alignment_filters_for_track(track_id).alignment_density)
    }

    pub(crate) fn set_read_panel_density(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        density: f32,
    ) -> bool {
        let density = density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
        let default_density = self.alignment_filters_for_track(track_id).alignment_density;
        let Some(panel) = self.panels.get_mut(panel_id) else {
            return false;
        };
        let state = panel.read_panel_scrolls.entry(track_id).or_default();
        let previous = state.density.unwrap_or(default_density);
        state.density = Some(density);
        let changed = (previous - density).abs() > f32::EPSILON;
        if changed {
            self.invalidate_navigation_contract("read density changed");
        }
        changed
    }

    pub(crate) fn primary_filter_editor(&self) -> &FilterEditor {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.filter_editor, |track| {
                &track.filter_editor
            })
    }

    pub(crate) fn primary_filter_editor_mut(&mut self) -> &mut FilterEditor {
        self.alignment_tracks
            .first_mut()
            .map_or(&mut self.empty_alignment.filter_editor, |track| {
                &mut track.filter_editor
            })
    }

    pub(crate) fn primary_active_filters(&self) -> &ActiveFilters {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.active_filters, |track| {
                &track.active_filters
            })
    }

    pub(crate) fn primary_active_filters_mut(&mut self) -> &mut ActiveFilters {
        self.alignment_tracks
            .first_mut()
            .map_or(&mut self.empty_alignment.active_filters, |track| {
                &mut track.active_filters
            })
    }

    pub(crate) fn primary_alignment_name(&self) -> &str {
        self.alignment_tracks
            .first()
            .map_or(&self.empty_alignment.name, |track| &track.config.name)
    }

    pub(crate) fn primary_alignment_visible(&self) -> bool {
        self.alignment_tracks
            .first()
            .map_or(self.empty_alignment.visible, |track| track.config.visible)
    }

    pub(crate) fn primary_alignment_collapsed(&self) -> bool {
        self.alignment_tracks
            .first()
            .map_or(self.empty_alignment.collapsed, |track| {
                track.config.collapsed
            })
    }

    pub(crate) fn primary_alignment_limit(&self) -> usize {
        self.alignment_tracks
            .first()
            .map_or(self.empty_alignment.maximum_alignments, |track| {
                track.config.maximum_alignments
            })
    }

    pub(crate) fn primary_alignment_source(&self) -> Option<&TrackSource> {
        self.alignment_tracks
            .first()
            .map(|track| &track.config.source)
    }

    pub(crate) fn primary_hidden_modification_codes(&self) -> Vec<ModificationCode> {
        self.primary_alignment_filters()
            .hidden_modification_codes
            .iter()
            .filter_map(|value| parse_modification_code(value))
            .take(MAX_HIDDEN_MODIFICATION_CODES)
            .collect()
    }

    pub(crate) fn set_primary_alignment_name(&mut self, name: String) {
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.config.name = name;
        } else {
            self.empty_alignment.name = name;
        }
    }

    pub(crate) fn set_primary_alignment_visible(&mut self, visible: bool) {
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.config.visible = visible;
        } else {
            self.empty_alignment.visible = visible;
        }
    }

    pub(crate) fn set_primary_alignment_collapsed(&mut self, collapsed: bool) {
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.config.collapsed = collapsed;
        } else {
            self.empty_alignment.collapsed = collapsed;
        }
    }

    pub(crate) fn set_primary_alignment_limit(&mut self, maximum_alignments: usize) {
        let (workspace_limit, maximum_alignments) =
            expanded_workspace_alignment_limit(self.workspace_alignment_limit, maximum_alignments);
        self.workspace_alignment_limit = workspace_limit;
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.config.maximum_alignments = maximum_alignments;
        } else {
            self.empty_alignment.maximum_alignments = maximum_alignments;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AlignmentSceneBudgetSnapshot {
    pub(crate) budget_bytes: usize,
    pub(crate) transition_reserve_bytes: usize,
    pub(crate) transient_budget_bytes: usize,
    pub(crate) charged_bytes: usize,
    pub(crate) reserved_bytes: usize,
    pub(crate) reserved_transition_bytes: usize,
    pub(crate) transient_bytes: usize,
    pub(crate) transient_high_water_bytes: usize,
    pub(crate) pinned_bytes: usize,
    pub(crate) pinned_overage_bytes: usize,
    pub(crate) active_replacement_bundle: Option<u64>,
    pub(crate) refreshes: u64,
}

impl AlignmentSceneBudgetSnapshot {
    pub(crate) fn from_progress(progress: LedgerProgress, refreshes: u64) -> Self {
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

pub(crate) fn alignment_panel_projections(
    panel_ids: &[PanelId],
) -> HashMap<PanelId, AlignmentPanelProjection> {
    panel_ids
        .iter()
        .copied()
        .map(|id| (id, AlignmentPanelProjection::default()))
        .collect()
}

pub(crate) const MAXIMUM_COVERAGE_BINS: usize = 4_096;
pub(crate) const MAXIMUM_ALLELE_COVERAGE_BASES_PER_PIXEL: f64 = 2.0;

pub(crate) fn alignment_coverage_query_plan(
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
fn alignment_scene_source_query_options(
    config: &SessionAlignmentTrack,
    active_filters: &ActiveFilters,
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
        max_records: NonZeroUsize::new(config.maximum_alignments.max(1))
            .expect("the complete read limit is non-zero"),
        collect_alleles: false,
        collect_modifications: false,
        overlapping_mate_policy: filters.overlapping_mate_policy,
        minimum_base_quality: filters.minimum_base_quality,
        minimum_junction_flank: filters.minimum_junction_flank,
        include_secondary: active_filters.include_secondary,
        include_supplementary: active_filters.include_supplementary,
        include_reference: true,
        filter: active_filters.alignment.clone(),
        ..QueryOptions::default()
    }
}

pub(crate) fn alignment_scene_build_contract(
    config: &SessionAlignmentTrack,
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
    let geometry_lod = match detail {
        DetailLevel::Overview => GeometryLod::Overview,
        DetailLevel::Regional => GeometryLod::Span,
        DetailLevel::Reads => GeometryLod::Read,
        DetailLevel::Bases => GeometryLod::Base,
    };
    let scene_reference_revision = if allele_detail != AlleleSummaryDetail::None {
        reference_revision
    } else {
        alignment_scene_reference_revision(geometry_lod, reference_revision)
    };
    AlignmentSceneBuildContract {
        source_revision: metadata.source_revision,
        record_contract: metadata.record_contract,
        contig,
        contig_length,
        // Complete record inspection/export is a product consumer. Retain the full payload once
        // in canonical evidence rather than creating a second query authority on demand.
        payload: PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        ),
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
            sort_anchor: alignment_sort_position(
                filters.alignment_sort_mode,
                filters.alignment_sort_anchor,
                viewport,
            ),
            view_as_pairs: filters.view_as_pairs,
            pair_length_bounds: TemplateLengthBounds {
                minimum: filters.minimum_template_length,
                maximum: filters.maximum_template_length,
            },
            promoted_template,
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

/// Base geometry compares retained read sequence against an immutable FASTA authority. Coarser
/// geometry stays reference-independent so opening or replacing a FASTA cannot invalidate the
/// resident Span/Read stack.
pub(crate) const fn alignment_scene_reference_revision(
    geometry_lod: GeometryLod,
    reference_revision: Option<ReferenceRevision>,
) -> Option<ReferenceRevision> {
    if matches!(geometry_lod, GeometryLod::Base) {
        reference_revision
    } else {
        None
    }
}

pub(crate) fn alignment_scene_preparation_policy(
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

pub(crate) fn alignment_scene_contract_equivalent(
    left: &AlignmentSceneBuildContract,
    right: &AlignmentSceneBuildContract,
) -> bool {
    left.source_revision == right.source_revision
        && left.record_contract == right.record_contract
        && left.contig == right.contig
        && left.contig_length == right.contig_length
        && left.payload == right.payload
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

#[cfg(test)]
pub(crate) fn alignment_query_needs_reference(
    color_mode: AlignmentColorMode,
    detail_level: DetailLevel,
    bases_per_pixel: f64,
) -> bool {
    bases_per_pixel <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
        || (detail_level == DetailLevel::Reads && alignment_color_mode_needs_reference(color_mode))
}

pub(crate) const fn alignment_color_mode_needs_reference(color_mode: AlignmentColorMode) -> bool {
    matches!(
        color_mode,
        AlignmentColorMode::Bisulfite(_) | AlignmentColorMode::Ultima(_)
    )
}

#[cfg(test)]
pub(crate) fn updated_alignment_selection(
    selected: &[TrackId],
    id: TrackId,
    additive: bool,
) -> Vec<TrackId> {
    if !additive {
        return vec![id];
    }
    let mut next = selected.to_vec();
    if let Some(index) = next.iter().position(|candidate| *candidate == id) {
        next.remove(index);
    } else {
        next.push(id);
    }
    next
}

#[cfg(test)]
pub(crate) fn alignment_range_selection(
    track_order: &[TrackId],
    anchor: TrackId,
    clicked: TrackId,
) -> Vec<TrackId> {
    let Some(anchor_index) = track_order.iter().position(|id| *id == anchor) else {
        return vec![clicked];
    };
    let Some(clicked_index) = track_order.iter().position(|id| *id == clicked) else {
        return vec![clicked];
    };
    let (start, end) = if anchor_index <= clicked_index {
        (anchor_index, clicked_index)
    } else {
        (clicked_index, anchor_index)
    };
    track_order[start..=end].to_vec()
}

#[cfg(test)]
pub(crate) fn move_ordered_item<T>(
    items: &mut Vec<T>,
    old_index: usize,
    target_index: usize,
) -> bool {
    if old_index >= items.len() {
        return false;
    }
    let new_index = target_index.min(items.len().saturating_sub(1));
    if old_index == new_index {
        return false;
    }
    let item = items.remove(old_index);
    items.insert(new_index, item);
    true
}

pub(crate) fn commit_alignment_settings_draft(
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
    let filters_changed = old_filters != update.filters;
    let limit = update.maximum_alignments.min(workspace_limit).max(1);
    query_changed |= old_limit != limit;
    *filter_editor = update.filter_editor;
    config.filters = update.filters;
    config.maximum_alignments = limit;
    AlignmentSettingsOutcome {
        configuration_changed: filters_changed || old_limit != limit,
        query_changed,
    }
}

pub(crate) fn copy_compatible_alignment_appearance(
    target: &mut SessionFilters,
    source: &SessionFilters,
) {
    target.alignment_color_mode = source.alignment_color_mode;
    target.alignment_group_mode = source.alignment_group_mode;
    target.alignment_group_order = source.alignment_group_order;
    target.alignment_sort_mode = source.alignment_sort_mode;
    target.alignment_sort_order = source.alignment_sort_order;
    target.alignment_sort_anchor = source.alignment_sort_anchor;
    target.read_stacking_mode = source.read_stacking_mode;
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

pub(crate) fn spawn_alignment_track_controller(
    config: SessionAlignmentTrack,
    cache_bytes: NonZeroUsize,
    scene_ledger: AllocationLedger,
    reference_path: Option<PathBuf>,
    remote_reference_index_url: Option<String>,
    panel_ids: &[PanelId],
) -> Result<AlignmentTrackController> {
    let (filter_editor, active_filters) = filter_state_from_session(&config.filters)?;
    let (scene_engine, lifecycle) = match &config.source {
        TrackSource::Local { path }
            if Path::new(path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bam")) =>
        {
            let query_options = alignment_scene_source_query_options(&config, &active_filters);
            (
                Some(AlignmentSceneEngine::spawn_local_bam(
                    LocalAlignmentSceneEngineConfig {
                        path: PathBuf::from(path),
                        open_options: alignment_open_options(
                            reference_path,
                            config.remote_index_url.clone(),
                            remote_reference_index_url,
                        ),
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
            )
        }
        TrackSource::Local { .. } => (
            None,
            AlignmentTrackLifecycle::Failed(
                "The unified alignment scene currently supports local BAM sources; this source was not opened and no legacy fallback was used"
                    .to_owned(),
            ),
        ),
        TrackSource::Remote { .. } => (
            None,
            AlignmentTrackLifecycle::Failed(
                "Remote alignment scene support is not qualified yet; this source was not opened and no legacy fallback was used"
                    .to_owned(),
            ),
        ),
    };
    let scene_complete_read_limit = config.maximum_alignments.max(1);
    Ok(AlignmentTrackController {
        config,
        scene_engine,
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

pub(crate) fn initial_filters(
    session: Option<&SessionDocument>,
) -> Result<(FilterEditor, ActiveFilters)> {
    let filters = session
        .and_then(primary_alignment_track)
        .map_or_else(SessionFilters::default, |track| track.filters.clone());
    filter_state_from_session(&filters)
}

pub(crate) fn filter_state_from_session(
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

pub(crate) fn active_filters_from_session(filters: &SessionFilters) -> Result<ActiveFilters> {
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
