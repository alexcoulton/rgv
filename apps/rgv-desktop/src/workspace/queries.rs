use super::super::*;
use super::alignment_management::alignment_scene_frame_for_ui;

/// A FASTA completion is authoritative only for the exact open handle and request it was sent
/// for. Generation acceptance is checked separately by the owning panel.
pub(crate) fn reference_query_completion_matches(
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
pub(crate) fn reference_failure_matches_requirement(
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
pub(crate) fn reference_dependency_is_settled(
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
pub(crate) fn alignment_scene_event_token_is_current(
    desired: Option<AlignmentSceneRequestToken>,
    event: Option<AlignmentSceneRequestToken>,
) -> bool {
    desired.is_some() && desired == event
}

/// A published frame may update a projection only when it belongs to the exact desktop
/// transaction and has reached that transaction's camera. Contract and policy controls share the
/// request token with the following camera control, so the token alone does not make an interim
/// old-camera frame current.
pub(crate) fn alignment_scene_frame_is_current(
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
pub(crate) fn alignment_scene_refusal_is_current(
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
pub(crate) fn alignment_scene_pressure_is_current(
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
pub(crate) fn alignment_scene_budget_event_is_current(
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
pub(crate) fn alignment_scene_pressure_requires_resume(
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
pub(crate) const fn alignment_scene_pressure_owns_navigation(
    phase: AlignmentNavigationPhase,
) -> bool {
    matches!(
        phase,
        AlignmentNavigationPhase::Running | AlignmentNavigationPhase::WaitingForSourcePressure
    )
}

/// An intrinsically oversized optional row-residency page limits instant deep scrolling; it does
/// not invalidate the already complete visible scene. Keep this operation check deliberately
/// narrow: speculative source acquisition has different lifecycle semantics and continues to use
/// its existing yield/terminal handling.
pub(crate) fn alignment_scene_pressure_preserves_visible_epoch(
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
pub(crate) enum AlignmentSceneFrameSettlement {
    Settled,
    SettledWithReferencePending,
    WaitingForReference,
    WaitingForScene,
}

pub(crate) fn alignment_scene_frame_settlement(
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
pub(crate) struct AlignmentReferenceLaneRequirement {
    pub(crate) owner: SourceOwnerId,
    pub(crate) revision: ReferenceRevision,
    pub(crate) horizon: GenomicInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AlignmentReferenceLanePlan {
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
pub(crate) fn plan_alignment_reference_lanes(
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

impl Viewer {
    fn set_panel_alignment_optional_work_paused(&mut self, panel_id: PanelId, paused: bool) {
        for track_index in 0..self.alignment_tracks.len() {
            let Some(owner) = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| {
                    (projection.scene_optional_work_paused != paused)
                        .then_some(projection.scene_owner)
                        .flatten()
                })
            else {
                continue;
            };
            let result = self.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| engine.try_set_optional_work_paused(owner, paused));
            match result {
                Some(Ok(())) => {
                    self.alignment_tracks[track_index]
                        .projection_mut(panel_id)
                        .scene_optional_work_paused = paused;
                }
                Some(Err(EngineQueueError::Disconnected)) => {
                    self.alignment_tracks[track_index].lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
                Some(Err(EngineQueueError::Full)) | None => {}
            }
        }
    }

    pub(crate) fn resume_alignment_optional_work_for_settled_panels(&mut self) {
        let settled = self
            .panels
            .iter()
            .map(|panel| panel.id)
            .filter(|panel_id| self.panel_alignment_queries_are_settled(*panel_id))
            .collect::<Vec<_>>();
        for panel_id in settled {
            self.set_panel_alignment_optional_work_paused(panel_id, false);
        }
    }

    fn mark_active_panel_navigation_queries_pending(&mut self) {
        let panel_id = self.panels.active().id;
        for track_index in 0..self.alignment_tracks.len() {
            self.queue_alignment_navigation_for_track(
                track_index,
                panel_id,
                false,
                "viewport navigation requested",
            );
        }
        let alignment_pending = self.alignment_tracks.iter().any(|track| {
            track
                .projection(panel_id)
                .is_some_and(|projection| projection.navigation.is_queued())
        });
        let base_marks_visible =
            self.panels.active().viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL;
        let reference_pending = self.reference_engine.is_some() && base_marks_visible;
        let tracks_pending = self.track_engine.is_some();
        let panel = self.panels.active_mut();
        panel.pending_queries.alignment = alignment_pending;
        panel.pending_queries.reference = reference_pending;
        panel.pending_queries.tracks = tracks_pending;
        panel.reference_request_revision = None;
        panel.reference_request_interval = None;
        panel.reference_installs_queued.clear();
        panel.track_request_interval = None;
        panel.reference_query_error_generation = None;
        panel.track_query_error_generations.clear();
    }

    pub(crate) fn begin_viewport_navigation(
        &mut self,
        gesture_active: bool,
        hold_commit_until_end: bool,
    ) {
        let panel_id = self.panels.active().id;
        if self
            .panels
            .active()
            .navigation_presentation
            .targets_viewport(self.panels.active().viewport)
        {
            return;
        }
        self.set_panel_alignment_optional_work_paused(panel_id, true);
        self.revoke_alignment_gpu_optional_residency_for_foreground();
        if self.synthetic
            || !(self.has_alignment_engine()
                || self.reference_engine.is_some()
                || self.track_engine.is_some())
        {
            self.rebuild_genomic_display();
            self.mark_display_dirty("explicit display invalidation");
            return;
        }

        let now = Instant::now();
        self.retain_active_read_selection_for_query_refresh();
        let previous_target_generation = self
            .panels
            .active()
            .navigation_presentation
            .target_generation;
        let previous_phase = self.panels.active().navigation_presentation.phase;
        self.panels.active().generation_clock.advance();
        self.advance_alignment_generations(panel_id);
        self.mark_active_panel_navigation_queries_pending();
        let generation = self.panels.active().generation_clock.current().get();
        let viewport = self.panels.active().viewport;
        let target = NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(viewport),
            generation,
        };
        let source_status = self.data_status.clone();
        let panel = self.panels.active_mut();
        if previous_phase == NavigationPresentationPhase::Acquiring
            && previous_target_generation != generation
        {
            panel.navigation_query_schedule.record_superseded_target();
        }
        panel.navigation_presentation.begin_navigation(
            viewport,
            generation,
            self.workspace_layout.revision,
            self.navigation_contract_revision,
            &source_status,
            now,
        );
        panel
            .navigation_query_schedule
            .update(target, now, gesture_active, hold_commit_until_end);
        // Context lanes retain immutable buffered geometry and move immediately under the desired
        // camera. Acquisition remains independently scheduled below and replaces only the lane
        // whose new horizon becomes ready.
        self.reproject_active_context_lanes();
        if gesture_active {
            // Alignment cameras are not debounced with source queries. The bounded scene mailbox
            // replaces unread camera intents per panel, while the UI immediately reprojects the
            // resident immutable scene under this exact viewport.
            self.dispatch_alignment_query();
        } else {
            let target = self
                .panels
                .active_mut()
                .navigation_query_schedule
                .take_immediate(now, false);
            if let Some(target) = target {
                self.dispatch_navigation_query_target(panel_id, target);
            }
        }
        self.window.request_redraw();
    }

    pub(crate) fn finish_navigation_gesture(&mut self) {
        let now = Instant::now();
        let panel_id = self.panels.active().id;
        let target = self
            .panels
            .active_mut()
            .navigation_query_schedule
            .take_immediate(now, true);
        if let Some(target) = target {
            self.dispatch_navigation_query_target(panel_id, target);
        }
        self.resume_alignment_optional_work_for_settled_panels();
        self.window.request_redraw();
    }

    fn dispatch_due_navigation_queries(&mut self, now: Instant) {
        let active_id = self.panels.active().id;
        let due = self
            .panels
            .iter_mut()
            .filter_map(|panel| {
                panel
                    .navigation_query_schedule
                    .take_due(now)
                    .map(|target| (panel.id, target))
            })
            .collect::<Vec<_>>();
        for (panel_id, target) in due {
            self.dispatch_navigation_query_target(panel_id, target);
        }
        self.panels.activate(active_id);
    }

    fn dispatch_navigation_query_target(
        &mut self,
        panel_id: PanelId,
        target: NavigationQueryTarget,
    ) {
        let Some(panel) = self.panels.get(panel_id) else {
            return;
        };
        if panel.generation_clock.current().get() != target.generation
            || NavigationTargetFingerprint::from_viewport(panel.viewport) != target.fingerprint
        {
            return;
        }
        self.panels.activate(panel_id);
        self.dispatch_alignment_query();
        self.dispatch_reference_query();
        self.dispatch_track_query();
    }

    pub(crate) fn panel_navigation_data_is_settled(&self, panel_id: PanelId) -> bool {
        let Some(panel) = self.panels.get(panel_id) else {
            return false;
        };
        if panel.navigation_query_schedule.pending.is_some() {
            return false;
        }
        // Alignment pixels own their own atomic scene epoch. Reference interpretation and generic
        // contextual tracks publish independently and cannot silently hold a complete alignment
        // target behind the panel navigation transaction.
        if !self.alignment_tracks.is_empty() {
            return self.panel_alignment_queries_are_settled(panel_id);
        }
        let panel_generation = panel.generation_clock.current().get();
        let reference_settled = reference_dependency_is_settled(
            self.reference_engine.is_some()
                && panel.viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
            panel_generation,
            self.reference_revision,
            panel.reference_slice_revision,
            panel.reference_slice.as_ref().map(|slice| slice.interval),
            panel.viewport.interval(),
            panel.reference_settled_generation,
            panel.reference_query_error_generation,
        );
        let tracks_settled =
            self.track_engine.is_none() || panel.track_settled_generation == Some(panel_generation);
        reference_settled && tracks_settled
    }

    fn panel_alignment_queries_are_settled(&self, panel_id: PanelId) -> bool {
        self.alignment_tracks
            .iter()
            .all(|track| match track.lifecycle {
                AlignmentTrackLifecycle::Opening => false,
                AlignmentTrackLifecycle::Ready => {
                    track.projection(panel_id).is_some_and(|projection| {
                        projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    })
                }
                AlignmentTrackLifecycle::Failed(_) => true,
            })
    }

    fn update_navigation_readiness(&mut self) {
        let candidate_ids = self
            .panels
            .iter()
            .filter(|panel| {
                panel.navigation_presentation.phase == NavigationPresentationPhase::Acquiring
            })
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        let active_id = self.panels.active().id;
        let mut ready = Vec::new();
        for panel_id in candidate_ids {
            self.panels.activate(panel_id);
            let data_settled = self.panel_navigation_data_is_settled(panel_id);
            let panel = self
                .panels
                .get(panel_id)
                .expect("navigation candidate belongs to the panel workspace");
            if navigation_target_ready_for_commit(
                panel.navigation_presentation.phase,
                panel.navigation_query_schedule.hold_commit_until_end,
                data_settled,
                true,
            ) {
                ready.push(panel_id);
            }
        }
        self.panels.activate(active_id);
        if ready.is_empty() {
            return;
        }
        for panel_id in ready {
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel.navigation_presentation.mark_ready_to_commit();
            }
        }
        self.mark_display_dirty("explicit display invalidation");
        self.window.request_redraw();
    }

    pub(crate) fn begin_ready_navigation_commits(&mut self) -> bool {
        let mut changed = false;
        for panel in self.panels.iter_mut() {
            changed |= panel.navigation_presentation.begin_commit();
        }
        changed
    }

    pub(crate) fn finish_navigation_commits(&mut self) -> bool {
        let now = Instant::now();
        let mut committed = false;
        for panel in self.panels.iter_mut() {
            let elapsed = now.saturating_duration_since(panel.navigation_presentation.started_at);
            if panel.navigation_presentation.finish_commit(
                self.workspace_layout.revision,
                self.navigation_contract_revision,
            ) {
                committed = true;
                panel.navigation_query_schedule.record_commit(elapsed);
            }
        }
        committed
    }

    pub(crate) fn invalidate_navigation_contract(&mut self, reason: &str) {
        self.navigation_contract_revision = self.navigation_contract_revision.saturating_add(1);
        for panel in self.panels.iter_mut() {
            panel
                .navigation_presentation
                .invalidate_contract(reason.to_owned());
        }
    }

    /// Starts a same-locus source refresh after a query/presentation contract change. Each live
    /// lane retains only scene pixels that remain valid under its own contract; there is no
    /// panel-wide neutral or retained-texture presentation.
    pub(crate) fn begin_all_panel_refresh_transactions(&mut self) {
        let now = Instant::now();
        let source_status = self.data_status.clone();
        for panel in self.panels.iter_mut() {
            panel.navigation_query_schedule.cancel();
            panel.navigation_presentation.begin_navigation(
                panel.viewport,
                panel.generation_clock.current().get(),
                self.workspace_layout.revision,
                self.navigation_contract_revision,
                &source_status,
                now,
            );
        }
    }

    pub(crate) fn schedule_resolution_query_refresh(&mut self) {
        self.pending_resolution_query_refresh = Some(Instant::now() + Duration::from_millis(150));
    }

    fn dispatch_due_resolution_query_refresh(&mut self, now: Instant) {
        if !self
            .pending_resolution_query_refresh
            .is_some_and(|deadline| deadline <= now)
        {
            return;
        }
        self.pending_resolution_query_refresh = None;
        let mut source_refresh_requested = false;
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            let resolution = AlignmentSceneResolution::for_viewport(self.panels.active().viewport);
            let alignment_refresh_required = self.alignment_tracks.iter().any(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.projection(id).is_none_or(|projection| {
                        projection
                            .last_scene_resolution
                            .is_none_or(|previous| resolution.requires_refresh_from(previous))
                    })
            });
            if alignment_refresh_required {
                self.retain_active_read_selection_for_query_refresh();
                self.advance_alignment_generations(id);
                self.request_alignment_query();
                source_refresh_requested = true;
            }
            let reference_missing = self.reference_engine.is_some()
                && self.panels.active().viewport.bases_per_pixel()
                    <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
                && (self.panels.active().reference_slice_revision != self.reference_revision
                    || self
                        .panels
                        .active()
                        .reference_slice
                        .as_ref()
                        .is_none_or(|slice| {
                            !slice
                                .interval
                                .contains_interval(self.panels.active().viewport.interval())
                        }));
            if reference_missing {
                self.request_reference_query();
                source_refresh_requested = true;
            }
        }
        self.panels.activate(active_id);
        if source_refresh_requested {
            self.resize_query_refreshes = self.resize_query_refreshes.saturating_add(1);
        }
    }

    pub(crate) fn request_track_query(&mut self) {
        self.panels.active_mut().pending_queries.tracks = self.track_engine.is_some();
        self.dispatch_track_query();
    }

    pub(crate) fn request_reference_query(&mut self) {
        let base_marks_visible = self.panels.active_mut().viewport.bases_per_pixel()
            <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL;
        self.panels.active_mut().pending_queries.reference =
            self.reference_engine.is_some() && base_marks_visible;
        self.dispatch_reference_query();
    }

    /// Returns every live current lane which still requires the currently open FASTA revision.
    /// A terminal dependency failure settles the projection generation and therefore excludes it
    /// until the next navigation advances that generation and retries.
    fn current_alignment_reference_requirements(
        &self,
        panel_id: PanelId,
        revision: ReferenceRevision,
    ) -> Vec<AlignmentReferenceLaneRequirement> {
        if self.panels.get(panel_id).is_some_and(|panel| {
            panel.reference_query_error_generation == Some(panel.generation_clock.current().get())
        }) {
            return Vec::new();
        }
        self.alignment_tracks
            .iter()
            .filter(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.scene_engine.is_some()
            })
            .filter_map(|track| {
                let projection = track.projection(panel_id)?;
                let owner = projection.scene_owner?;
                let frame = projection.scene_frame.as_ref()?;
                if projection.navigation.desired_camera() != Some(frame.camera)
                    || !alignment_scene_event_token_is_current(
                        projection.navigation.request_token(),
                        projection.scene_frame_request_token,
                    )
                {
                    return None;
                }
                match frame.reference_requirement {
                    AlignmentReferenceRequirement::Pending {
                        revision: required_revision,
                        horizon,
                    } if required_revision == revision => Some(AlignmentReferenceLaneRequirement {
                        owner,
                        revision,
                        horizon,
                    }),
                    AlignmentReferenceRequirement::NotRequired
                    | AlignmentReferenceRequirement::Pending { .. }
                    | AlignmentReferenceRequirement::Ready { .. } => None,
                }
            })
            .collect()
    }

    /// Queues an already-fetched immutable FASTA slice into every current scene owner whose
    /// pending requirement it satisfies. Success means every live current owner is either already
    /// satisfied or has accepted its exact authority into the bounded engine control queue.
    fn install_reference_authority_for_panel(
        &mut self,
        panel_id: PanelId,
        revision: ReferenceRevision,
        slice: &Arc<ReferenceSlice>,
    ) -> bool {
        let authority = AlignmentReferenceAuthority::new(revision, Arc::clone(slice));
        let requirements = self.current_alignment_reference_requirements(panel_id, revision);
        let mut queued = self
            .panels
            .get_mut(panel_id)
            .map(|panel| std::mem::take(&mut panel.reference_installs_queued))
            .unwrap_or_default();
        queued.retain(|owner, queued_requirement| {
            requirements.iter().any(|requirement| {
                requirement.owner == *owner
                    && *queued_requirement == (requirement.revision, requirement.horizon)
            })
        });
        for requirement in &requirements {
            if queued.get(&requirement.owner) == Some(&(requirement.revision, requirement.horizon))
                || !slice.interval.contains_interval(requirement.horizon)
            {
                continue;
            }
            let Some(track_index) = self.alignment_tracks.iter().position(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.scene_owner == Some(requirement.owner))
            }) else {
                continue;
            };
            let result = self.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| {
                    engine.try_install_reference_authority(requirement.owner, authority.clone())
                });
            match result {
                Some(Ok(())) => {
                    queued.insert(
                        requirement.owner,
                        (requirement.revision, requirement.horizon),
                    );
                }
                Some(Err(EngineQueueError::Full)) | None => {}
                Some(Err(EngineQueueError::Disconnected)) => {
                    // This lane is terminal and will no longer participate in the panel's
                    // reference settlement. Do not leave the otherwise-successful panel retrying
                    // an engine which has already disconnected.
                    self.alignment_tracks[track_index].lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
            }
        }
        let live = self.current_alignment_reference_requirements(panel_id, revision);
        queued.retain(|owner, queued_requirement| {
            live.iter().any(|requirement| {
                requirement.owner == *owner
                    && *queued_requirement == (requirement.revision, requirement.horizon)
            })
        });
        let all_queued = matches!(
            plan_alignment_reference_lanes(&live, &queued),
            AlignmentReferenceLanePlan::AllQueued
        );
        if let Some(panel) = self.panels.get_mut(panel_id) {
            panel.reference_installs_queued = queued;
        }
        all_queued
    }

    pub(crate) fn dispatch_reference_query(&mut self) {
        if !self.panels.active().pending_queries.reference {
            return;
        }
        let panel_id = self.panels.active().id;
        let Some(revision) = self.reference_revision else {
            self.panels.active_mut().pending_queries.reference = false;
            return;
        };
        let reference_contig_name = match self.compatible_reference_contig_for_panel(panel_id) {
            Ok(Some(name)) => name,
            Ok(None) => {
                self.panels.active_mut().pending_queries.reference = false;
                return;
            }
            Err(reason) => {
                let panel = self.panels.active_mut();
                panel.pending_queries.reference = false;
                panel.reference_slice = None;
                panel.reference_slice_revision = None;
                panel.reference_slice_generation = None;
                panel.reference_request_revision = None;
                panel.reference_request_interval = None;
                panel.reference_installs_queued.clear();
                panel.reference_settled_generation = Some(panel.generation_clock.current().get());
                panel.reference_query_error_generation =
                    Some(panel.generation_clock.current().get());
                self.data_status = format!(
                    "Reference incompatible with alignment: {reason}. Reference-dependent bases and substitutions are disabled"
                );
                return;
            }
        };
        let cached = self.panels.get(panel_id).and_then(|panel| {
            (panel.reference_slice_revision == Some(revision))
                .then(|| panel.reference_slice.as_ref())
                .flatten()
                .cloned()
        });
        if let Some(slice) = &cached {
            self.install_reference_authority_for_panel(panel_id, revision, slice);
        }
        let requirements = self.current_alignment_reference_requirements(panel_id, revision);
        let lane_plan = self
            .panels
            .get(panel_id)
            .map(|panel| {
                plan_alignment_reference_lanes(&requirements, &panel.reference_installs_queued)
            })
            .unwrap_or(AlignmentReferenceLanePlan::AllQueued);
        let contextual_interval = buffered_context_interval(
            self.panels.active().viewport,
            self.panels.active().contig_length,
            self.settings.read_browsing_buffer_percent,
            Some(MAX_REFERENCE_QUERY_BASES),
        );
        let cached_covers_context = cached
            .as_ref()
            .is_some_and(|slice| slice.interval.contains_interval(contextual_interval));
        if matches!(lane_plan, AlignmentReferenceLanePlan::AllQueued) && cached_covers_context {
            let panel = self.panels.active_mut();
            panel.pending_queries.reference = false;
            let generation = panel.generation_clock.current().get();
            panel.reference_slice_generation = Some(generation);
            panel.reference_settled_generation = Some(generation);
            panel.reference_query_error_generation = None;
            return;
        }
        let (revision, interval) = match lane_plan {
            AlignmentReferenceLanePlan::Query { revision, interval } => (revision, interval),
            AlignmentReferenceLanePlan::AllQueued => (revision, contextual_interval),
        };
        if cached
            .as_ref()
            .is_some_and(|slice| slice.interval.contains_interval(interval))
        {
            // The bases are already resident, so an unqueued owner can only be waiting for bounded
            // control-queue capacity. Keep the dispatch flag set and retry without duplicate I/O.
            self.panels.active_mut().pending_queries.reference = true;
            return;
        }
        let already_requested = self.panels.get(panel_id).is_some_and(|panel| {
            panel.reference_request_revision == Some(revision)
                && panel.reference_request_interval == Some(interval)
        });
        if already_requested {
            self.panels.active_mut().pending_queries.reference = false;
            return;
        }
        let Some(engine) = &self.reference_engine else {
            self.panels.active_mut().pending_queries.reference = false;
            return;
        };
        let query = ReferenceQueryIdentity::new(self.panels.active().request(), revision, interval);
        match engine.try_query(
            query,
            self.panels.active_mut().generation_clock.clone(),
            reference_contig_name,
        ) {
            Ok(()) => {
                let panel = self.panels.active_mut();
                panel.reference_request_revision = Some(revision);
                panel.reference_request_interval = Some(interval);
                panel.pending_queries.reference = false;
            }
            Err(EngineQueueError::Full) => {
                "Reference queue busy; retrying the latest locus…"
                    .clone_into(&mut self.data_status);
            }
            Err(EngineQueueError::Disconnected) => {
                self.panels.active_mut().pending_queries.reference = false;
                self.report_error(EngineQueueError::Disconnected.to_string());
            }
        }
    }

    pub(crate) fn request_all_panel_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.retain_active_read_selection_for_query_refresh();
            self.request_alignment_query();
            self.request_reference_query();
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn request_all_alignment_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.retain_active_read_selection_for_query_refresh();
            self.request_alignment_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn request_all_auxiliary_track_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn invalidate_and_request_all_auxiliary_track_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.panels.active().generation_clock.advance();
            self.request_reference_query();
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn dispatch_track_query(&mut self) {
        if !self.panels.active().pending_queries.tracks {
            return;
        }
        let Some(engine) = &self.track_engine else {
            self.panels.active_mut().pending_queries.tracks = false;
            return;
        };
        let interval = buffered_context_interval(
            self.panels.active().viewport,
            self.panels.active().contig_length,
            self.settings.read_browsing_buffer_percent,
            None,
        );
        let request = self.panels.active().request();
        let track_ids = auxiliary_query_track_ids(self.tracks.tracks());
        let queried = track_ids.iter().copied().collect::<HashSet<_>>();
        let variant_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Variant && queried.contains(&track.id))
            .map(|track| (track.id, track.variant.clone()))
            .collect();
        let interaction_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Interaction && queried.contains(&track.id))
            .map(|track| (track.id, track.interaction.clone()))
            .collect();
        let copy_number_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::CopyNumber && queried.contains(&track.id))
            .map(|track| (track.id, track.copy_number.clone()))
            .collect();
        match engine.try_query_selected_with_settings(
            request,
            self.panels.active_mut().generation_clock.clone(),
            self.panels.active_mut().contig_name.clone(),
            interval,
            track_ids,
            variant_settings,
            interaction_settings,
            copy_number_settings,
        ) {
            Ok(()) => {
                let panel = self.panels.active_mut();
                panel.pending_queries.tracks = false;
                panel.track_request_interval = Some(interval);
            }
            Err(EngineQueueError::Full) => {
                "Track queue busy; retrying the latest locus…".clone_into(&mut self.data_status);
            }
            Err(EngineQueueError::Disconnected) => {
                self.panels.active_mut().pending_queries.tracks = false;
                self.panels.active_mut().track_request_interval = None;
                self.report_error(EngineQueueError::Disconnected.to_string());
            }
        }
    }

    pub(crate) fn retry_pending_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            let alignment_pending = self.alignment_tracks.iter().any(|track| {
                track
                    .projection(id)
                    .is_some_and(|projection| projection.navigation.is_queued())
            });
            self.panels.activate(id);
            if self
                .panels
                .active()
                .navigation_query_schedule
                .pending
                .is_some()
            {
                continue;
            }
            self.panels.active_mut().pending_queries.alignment = alignment_pending;
            self.dispatch_alignment_query();
            self.dispatch_reference_query();
            self.dispatch_track_query();
        }
        self.panels.activate(active_id);
    }

    /// Resumes paused, exact source intents only after their typed ledger obstruction has cleared.
    /// The epoch is a cheap change detector, not proof of capacity: unrelated ledger churn is
    /// consumed without reopening the same request.
    fn retry_deferred_alignment_source_pressures(&mut self) {
        if !self.alignment_tracks.iter().any(|track| {
            track
                .projections
                .values()
                .any(|projection| projection.scene_pressure_retry.is_some())
        }) {
            return;
        }
        let Ok(snapshot) = self.alignment_scene_ledger.progress() else {
            return;
        };
        let epoch = snapshot.progress_epoch();
        let mut waiting_updates = Vec::new();
        let mut retries = Vec::new();
        for (track_index, track) in self.alignment_tracks.iter().enumerate() {
            if !matches!(track.lifecycle, AlignmentTrackLifecycle::Ready) {
                continue;
            }
            for (panel_id, projection) in &track.projections {
                let Some(retry) = projection.scene_pressure_retry else {
                    continue;
                };
                let Some(owner) = projection.scene_owner else {
                    continue;
                };
                if !alignment_scene_pressure_is_current(
                    projection.navigation.request_token(),
                    retry.request_token,
                    projection.navigation.desired_camera(),
                    retry.target,
                ) {
                    continue;
                }
                match retry.poll(&snapshot) {
                    AlignmentScenePressureRetryPoll::Waiting(updated) => {
                        if updated != retry {
                            waiting_updates.push((track_index, *panel_id, retry, updated));
                        }
                    }
                    AlignmentScenePressureRetryPoll::Ready => retries.push((
                        track_index,
                        *panel_id,
                        owner,
                        retry.request_token,
                        retry.target,
                    )),
                }
            }
        }

        for (track_index, panel_id, observed, updated) in waiting_updates {
            if let Some(retry) = self.alignment_tracks[track_index]
                .projection_mut(panel_id)
                .scene_pressure_retry
                .as_mut()
            {
                // Preserve a newer engine response if one was consumed between collection and
                // mutation (the desktop currently drains serially, but this keeps the authority
                // local and explicit).
                if *retry == observed {
                    *retry = updated;
                }
            }
        }

        for (track_index, panel_id, owner, request_token, target) in retries {
            let result = self.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| engine.try_retry_resource_pressure(owner, request_token, target));
            {
                let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
                if let Some(retry) = projection.scene_pressure_retry.as_mut()
                    && retry.same_request(request_token, target)
                {
                    retry.ledger_epoch = epoch;
                    retry.in_flight = matches!(result, Some(Ok(())));
                }
                if matches!(result, Some(Ok(())))
                    && projection.navigation.phase()
                        == AlignmentNavigationPhase::WaitingForSourcePressure
                {
                    projection.navigation.transition(
                        AlignmentNavigationPhase::Running,
                        "ledger progress resumed the exact source target",
                    );
                }
            }
            if matches!(result, Some(Err(EngineQueueError::Disconnected)) | None) {
                self.alignment_tracks[track_index]
                    .projection_mut(panel_id)
                    .settle_failed("alignment source mailbox disconnected during pressure retry");
                self.alignment_tracks[track_index].lifecycle =
                    AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                self.alignment_tracks[track_index].scene_budget_retry = None;
            }
        }
    }

    /// Replays a source coordinator's latest refused budget edit exactly once after its prepared
    /// replacement obstruction clears. Every source still runs its own eviction pass even though
    /// all sources and renderers share one enforcing ledger.
    fn retry_deferred_alignment_budget_pressures(&mut self) {
        if !self
            .alignment_tracks
            .iter()
            .any(|track| track.scene_budget_retry.is_some())
        {
            return;
        }
        let Ok(snapshot) = self.alignment_scene_ledger.progress() else {
            return;
        };
        let desired_budget = self.alignment_scene_budget.budget_bytes;
        let mut stale = Vec::new();
        let mut waiting_updates = Vec::new();
        let mut retries = Vec::new();
        for (track_index, track) in self.alignment_tracks.iter().enumerate() {
            let Some(retry) = track.scene_budget_retry else {
                continue;
            };
            if !alignment_scene_budget_event_is_current(
                desired_budget,
                track.scene_budget_bytes.get(),
                retry.requested_budget,
            ) {
                stale.push(track_index);
                continue;
            }
            match retry.poll(&snapshot) {
                AlignmentSceneBudgetRetryPoll::Waiting(updated) => {
                    if updated != retry {
                        waiting_updates.push((track_index, updated));
                    }
                }
                AlignmentSceneBudgetRetryPoll::Ready => {
                    retries.push((
                        track_index,
                        retry.requested_budget,
                        retry.clear_before_apply,
                    ));
                }
            }
        }

        for track_index in stale {
            self.alignment_tracks[track_index].scene_budget_retry = None;
        }
        for (track_index, updated) in waiting_updates {
            if self.alignment_tracks[track_index]
                .scene_budget_retry
                .is_some_and(|retry| retry.requested_budget == updated.requested_budget)
            {
                self.alignment_tracks[track_index].scene_budget_retry = Some(updated);
            }
        }
        for (track_index, requested_budget, clear_before_apply) in retries {
            let result = self.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| {
                    engine.try_retry_budget_pressure(requested_budget, clear_before_apply)
                });
            if let Some(retry) = self.alignment_tracks[track_index]
                .scene_budget_retry
                .as_mut()
                && retry.requested_budget == requested_budget
            {
                retry.ledger_epoch = snapshot.progress_epoch();
                retry.in_flight = matches!(result, Some(Ok(())));
            }
            if matches!(result, Some(Err(EngineQueueError::Disconnected)) | None) {
                self.alignment_tracks[track_index].lifecycle =
                    AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
            }
        }
    }

    pub(crate) fn poll_background_work(&mut self) {
        let now = Instant::now();
        self.dispatch_due_navigation_queries(now);
        self.dispatch_due_resolution_query_refresh(now);
        self.poll_reference_events();
        self.poll_alignment_events();
        let ledger_progressed = self.alignment_ledger_progress.take_pending();
        if ledger_progressed {
            self.stress_health.note_worker_progress();
        }
        let immediate_budget_retry = self.alignment_tracks.iter().any(|track| {
            track
                .scene_budget_retry
                .is_some_and(|retry| retry.ready_after_observed_progress)
        });
        let immediate_gpu_retry = self.alignment_gpu_publication_retry.is_some_and(|retry| {
            matches!(
                retry.gate,
                AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
            )
        });
        if ledger_progressed || immediate_budget_retry {
            self.retry_deferred_alignment_budget_pressures();
            self.retry_deferred_alignment_source_pressures();
        }
        if ledger_progressed || immediate_gpu_retry {
            self.retry_deferred_alignment_gpu_publication();
        }
        self.poll_track_events();
        self.synchronize_alignment_contextual_product_states();
        self.poll_alignment_retirement_tasks();
        self.poll_engine_retirement_tasks();
        self.poll_session_load_tasks();
        self.poll_session_save_tasks();
        self.poll_settings_writer();
        self.poll_export_tasks();
        self.poll_workspace_list_import_tasks();
        self.poll_genome_tasks();
        self.retry_pending_queries();
        self.update_navigation_readiness();
        if self.reveal_due_alignment_checking_notices(now)
            || self.reveal_due_navigation_notices(now)
        {
            self.mark_display_dirty("explicit display invalidation");
        }
        if self.display_dirty {
            self.window.request_redraw();
        }
        self.maybe_start_pending_manual_diagnostic_trace();
        let desired_generation = self
            .panels
            .iter()
            .map(|panel| panel.generation_clock.current().get())
            .max()
            .unwrap_or_default();
        let committed_generation = if self.automation_is_settled() {
            desired_generation
        } else {
            desired_generation.saturating_sub(1)
        };
        self.stress_health
            .observe_generations(desired_generation, committed_generation);
    }

    fn synchronize_alignment_contextual_product_states(&mut self) {
        let has_contextual_lanes = !self.tracks.tracks().is_empty();
        let states = self
            .panels
            .iter()
            .map(|panel| {
                let generation = panel.generation_clock.current().get();
                let state = if !has_contextual_lanes {
                    AlignmentNavigationProductState::Independent
                } else if panel
                    .track_query_error_generations
                    .values()
                    .any(|failed_generation| *failed_generation == generation)
                {
                    AlignmentNavigationProductState::IndependentFailed
                } else if panel.pending_queries.tracks
                    || panel.track_settled_generation != Some(generation)
                {
                    AlignmentNavigationProductState::IndependentPending
                } else {
                    AlignmentNavigationProductState::IndependentReady
                };
                (panel.id, state)
            })
            .collect::<Vec<_>>();
        for track in &mut self.alignment_tracks {
            for (panel_id, state) in &states {
                let reason = match state {
                    AlignmentNavigationProductState::Independent => {
                        "no contextual annotation lanes are required"
                    }
                    AlignmentNavigationProductState::IndependentPending => {
                        "contextual annotation lanes remain independently pending"
                    }
                    AlignmentNavigationProductState::IndependentReady => {
                        "contextual annotation lanes are independently ready"
                    }
                    AlignmentNavigationProductState::IndependentFailed => {
                        "contextual annotation lanes failed independently"
                    }
                    _ => unreachable!("contextual product synchronization uses independent states"),
                };
                track
                    .projection_mut(*panel_id)
                    .navigation
                    .observe_contextual_lanes(*state, reason);
            }
        }
    }

    fn reveal_due_navigation_notices(&mut self, now: Instant) -> bool {
        self.panels.iter_mut().fold(false, |changed, panel| {
            panel.navigation_presentation.reveal_notice_if_due(now) || changed
        })
    }

    fn reveal_due_alignment_checking_notices(&mut self, now: Instant) -> bool {
        self.alignment_tracks
            .iter_mut()
            .flat_map(|track| track.projections.values_mut())
            .fold(false, |changed, projection| {
                projection.reveal_checking_notice_if_due(now) || changed
            })
    }

    pub(crate) fn poll_alignment_retirement_tasks(&mut self) {
        let mut index = 0;
        while index < self.alignment_retirement_tasks.len() {
            if !self.alignment_retirement_tasks[index].is_finished() {
                index += 1;
                continue;
            }
            let task = self.alignment_retirement_tasks.remove(index);
            if task.join().is_err() {
                tracing::warn!("alignment reaper panicked");
            }
        }
    }

    pub(crate) fn finish_alignment_retirement_tasks(&mut self) {
        for task in self.alignment_retirement_tasks.drain(..) {
            if task.join().is_err() {
                tracing::warn!("alignment reaper panicked during shutdown");
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_reference_events(&mut self) {
        let mut events = Vec::new();
        if let Some(engine) = &self.reference_engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        self.report_error(error.to_string());
                        break;
                    }
                }
            }
        }
        if events.is_empty() {
            return;
        }
        let active_panel_id = self.panels.active().id;
        let mut reference_panels_to_retry = BTreeSet::new();
        let mut highlight_panels_to_synchronize = BTreeSet::new();
        for event in events {
            match event {
                ReferenceEngineEvent::Opened {
                    revision,
                    dictionary,
                    remote_io_stats,
                } => {
                    let reference_contig_count = dictionary.len();
                    let alignment_incompatibility =
                        self.primary_alignment_dictionary().and_then(|alignment| {
                            match alignment_reference_compatibility(
                                alignment,
                                &dictionary,
                                &self.contig_aliases,
                            ) {
                                AlignmentReferenceCompatibility::Compatible => None,
                                AlignmentReferenceCompatibility::Incompatible(reason) => {
                                    Some(reason)
                                }
                            }
                        });
                    self.reference_open_error = None;
                    self.reference_revision = Some(revision);
                    self.reference_remote_io_stats = remote_io_stats;
                    self.reference_dictionary = Some(dictionary.clone());
                    // Opening (including reopening the same path) establishes a new immutable
                    // authority. No slice or in-flight request from the previous handle may be
                    // paired with contracts carrying this revision.
                    for panel in self.panels.iter_mut() {
                        panel.reference_slice = None;
                        panel.reference_slice_revision = None;
                        panel.reference_slice_generation = None;
                        panel.reference_request_revision = None;
                        panel.reference_request_interval = None;
                        panel.reference_installs_queued.clear();
                        panel.reference_settled_generation = None;
                        panel.reference_query_error_generation = None;
                        panel.pending_queries.reference = false;
                    }
                    highlight_panels_to_synchronize
                        .extend(self.panels.iter().map(|panel| panel.id));
                    if self.primary_alignment_dictionary().is_none() {
                        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
                        let mut imported_feature_search = None;
                        for id in ids {
                            let (requested, pixel_width) = self
                                .panels
                                .get(id)
                                .map(|panel| {
                                    (panel.requested_locus.clone(), panel.viewport.pixel_width())
                                })
                                .expect("panel id came from the panel collection");
                            match self
                                .resolve_panel_requested_locus(&dictionary, requested.as_deref())
                                .and_then(|locus| {
                                    Ok((Viewport::new(locus.interval, pixel_width)?, locus))
                                }) {
                                Ok((viewport, locus)) => {
                                    let RequestedLocus {
                                        interval,
                                        contig_name: name,
                                        contig_length: length,
                                        feature_search,
                                    } = locus;
                                    imported_feature_search =
                                        imported_feature_search.or(feature_search);
                                    let panel = self
                                        .panels
                                        .get_mut(id)
                                        .expect("panel id came from the panel collection");
                                    panel.generation_clock.advance();
                                    panel.viewport = viewport;
                                    panel.contig_name = name;
                                    panel.contig_length = length;
                                    panel.reset_history(interval);
                                    panel.vertical_scroll_points = 0.0;
                                    panel.vertical_content_origin_points = 0.0;
                                    panel.latent_vertical_scroll_fraction = None;
                                }
                                Err(error) if id == active_panel_id => {
                                    self.report_error(error.to_string());
                                }
                                Err(_) => {}
                            }
                        }
                        self.finish_imported_locus(imported_feature_search);
                        let default_annotation_count = self.assembly.default_annotation_count;
                        let (assembly, cytoband_failure) = load_assembly_reporting(
                            &self.assembly_name,
                            dictionary.clone(),
                            self.cytoband_path.as_deref(),
                        );
                        self.assembly = assembly;
                        if let Some(failure) = cytoband_failure {
                            self.report_error(failure);
                        }
                        self.assembly.default_annotation_count = default_annotation_count;
                    }
                    self.update_landmark_resolution_from_dictionary(&dictionary);
                    self.assembly.reference_contig_count = reference_contig_count;
                    self.assembly.identity.clone_from(&self.assembly_identity);
                    self.synthetic = false;
                    if !self.has_alignment_engine() {
                        let source = self.reference_source.as_ref().map_or_else(
                            || "reference genome".to_owned(),
                            TrackSource::redacted_label,
                        );
                        self.data_status = format!("Opened {source}");
                    } else if let Some(reason) = alignment_incompatibility {
                        self.data_status = format!(
                            "Reference incompatible with alignment: {reason}. Reference-dependent bases and substitutions are disabled"
                        );
                    }
                    self.refresh_active_panel_context();
                    self.request_all_panel_queries();
                    self.mark_display_dirty("explicit display invalidation");
                }
                ReferenceEngineEvent::QueryCompleted {
                    query,
                    slice,
                    remote_io_stats,
                } => {
                    let request = query.request();
                    let revision = query.revision();
                    self.reference_remote_io_stats = remote_io_stats;
                    let is_active = request.panel_id() == active_panel_id;
                    let mut accepted = false;
                    let current_revision = self.reference_revision;
                    if let Some(panel) = self.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                        && reference_query_completion_matches(
                            current_revision,
                            panel.reference_request_revision,
                            panel.reference_request_interval,
                            revision,
                            query.interval(),
                        )
                        && query.interval() == slice.interval
                    {
                        accepted = true;
                        panel.reference_slice = Some(Arc::clone(&slice));
                        panel.reference_slice_revision = Some(revision);
                        panel.reference_slice_generation = Some(request.generation().get());
                        panel.reference_request_revision = None;
                        panel.reference_request_interval = None;
                        panel.reference_query_error_generation = None;
                        self.mark_display_dirty("explicit display invalidation");
                        if is_active && !self.has_alignment_engine() {
                            "Reference sequence loaded".clone_into(&mut self.data_status);
                        }
                    }
                    if accepted {
                        highlight_panels_to_synchronize.insert(request.panel_id());
                        let installed = self.install_reference_authority_for_panel(
                            request.panel_id(),
                            revision,
                            &slice,
                        );
                        let contextual_horizon = self.panels.get(request.panel_id()).map(|panel| {
                            buffered_context_interval(
                                panel.viewport,
                                panel.contig_length,
                                self.settings.read_browsing_buffer_percent,
                                Some(MAX_REFERENCE_QUERY_BASES),
                            )
                        });
                        let context_covered = contextual_horizon
                            .is_some_and(|interval| slice.interval.contains_interval(interval));
                        let settled = installed && context_covered;
                        if let Some(panel) = self.panels.get_mut(request.panel_id()) {
                            panel.pending_queries.reference = !settled;
                            if settled {
                                panel.reference_settled_generation =
                                    Some(request.generation().get());
                            }
                        }
                        if !settled {
                            reference_panels_to_retry.insert(request.panel_id());
                        }
                    } else {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                    }
                }
                ReferenceEngineEvent::Error { query, message } => {
                    if query.is_none() {
                        self.reference_open_error = Some(message.clone());
                        self.reference_revision = None;
                        self.reference_dictionary = None;
                        // A failed open is terminal for this actor. Leaving it installed would
                        // make every later Base navigation advertise reference work even though
                        // no open revision exists and no query can be issued.
                        self.retire_reference_engine();
                        for panel in self.panels.iter_mut() {
                            panel.settle_reference_open_failure();
                        }
                        for projection in self
                            .alignment_tracks
                            .iter_mut()
                            .flat_map(|track| track.projections.values_mut())
                        {
                            if projection.navigation.phase()
                                == AlignmentNavigationPhase::WaitingForReference
                            {
                                projection.settle_failed(
                                    "reference source failed while evaluating the alignment view filter",
                                );
                            } else {
                                projection.navigation.observe_reference_failure(
                                    "reference source failed; core alignment navigation remains independent",
                                );
                            }
                        }
                    }
                    let current = query.is_none_or(|query| {
                        let request = query.request();
                        self.panels.get(request.panel_id()).is_some_and(|panel| {
                            panel.accepts(request)
                                && reference_query_completion_matches(
                                    self.reference_revision,
                                    panel.reference_request_revision,
                                    panel.reference_request_interval,
                                    query.revision(),
                                    query.interval(),
                                )
                        })
                    });
                    if query.is_some() && !current {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                    }
                    if current {
                        let mut failed_authority = None;
                        if let Some(query) = query {
                            let request = query.request();
                            if let Some(panel) = self.panels.get_mut(request.panel_id()) {
                                failed_authority =
                                    Some((request.panel_id(), query.revision(), query.interval()));
                                panel.reference_request_revision = None;
                                panel.reference_request_interval = None;
                                panel.reference_settled_generation =
                                    Some(request.generation().get());
                                panel.reference_query_error_generation =
                                    Some(request.generation().get());
                            }
                        }
                        if let Some((panel_id, revision, interval)) = failed_authority {
                            let mut failed_owners = Vec::new();
                            for track in &mut self.alignment_tracks {
                                let projection = track.projection_mut(panel_id);
                                let matching_current_requirement =
                                    projection.scene_frame.as_ref().is_some_and(|frame| {
                                        projection.navigation.desired_camera() == Some(frame.camera)
                                            && alignment_scene_event_token_is_current(
                                                projection.navigation.request_token(),
                                                projection.scene_frame_request_token,
                                            )
                                            && reference_failure_matches_requirement(
                                                revision,
                                                interval,
                                                frame.reference_requirement,
                                            )
                                    });
                                if matching_current_requirement {
                                    if projection.navigation.phase()
                                        == AlignmentNavigationPhase::WaitingForReference
                                    {
                                        // Reference interpretation is optional for an already
                                        // complete scene, but it is a mandatory dependency of a
                                        // reference-backed view filter. Settle that exact request
                                        // truthfully instead of leaving it ownerless or showing
                                        // rows evaluated under the previous filter.
                                        projection.settle_failed(
                                            "reference query failed while evaluating the alignment view filter",
                                        );
                                    } else {
                                        projection.finish_checking();
                                        projection.navigation.observe_reference_failure(
                                            "reference query failed; core alignment navigation remains independent",
                                        );
                                    }
                                    if let Some(owner) = projection.scene_owner {
                                        failed_owners.push(owner);
                                    }
                                }
                            }
                            let alignment_pending = self.alignment_tracks.iter().any(|track| {
                                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
                                    && track.projection(panel_id).is_some_and(|projection| {
                                        !projection
                                            .navigation
                                            .is_current_settled(projection.clock.current().get())
                                    })
                            });
                            if let Some(panel) = self.panels.get_mut(panel_id) {
                                panel.pending_queries.alignment = alignment_pending;
                                for owner in failed_owners {
                                    panel.reference_installs_queued.remove(&owner);
                                }
                            }
                            let remaining =
                                self.current_alignment_reference_requirements(panel_id, revision);
                            let needs_another_query =
                                self.panels.get(panel_id).is_some_and(|panel| {
                                    matches!(
                                        plan_alignment_reference_lanes(
                                            &remaining,
                                            &panel.reference_installs_queued,
                                        ),
                                        AlignmentReferenceLanePlan::Query { .. }
                                    )
                                });
                            if needs_another_query {
                                if let Some(panel) = self.panels.get_mut(panel_id) {
                                    panel.pending_queries.reference = true;
                                }
                                reference_panels_to_retry.insert(panel_id);
                            }
                        }
                        self.data_status = message;
                    }
                }
            }
        }
        if !reference_panels_to_retry.is_empty() {
            let restore = self.panels.active().id;
            for panel_id in reference_panels_to_retry {
                self.panels.activate(panel_id);
                self.dispatch_reference_query();
            }
            self.panels.activate(restore);
        }
        if !highlight_panels_to_synchronize.is_empty() {
            for panel_id in highlight_panels_to_synchronize {
                self.synchronize_alignment_scene_interactions(panel_id);
            }
            self.rebuild_all_panel_displays();
            self.prepare_alignment_scene_frame();
        }
        self.window.request_redraw();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_alignment_events(&mut self) {
        let active_panel_id = self.panels.active().id;
        let primary_id = self.alignment_tracks.first().map(|track| track.config.id);
        let mut events = Vec::new();
        for track in &mut self.alignment_tracks {
            let track_id = track.config.id;
            let Some(engine) = &track.scene_engine else {
                continue;
            };
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push((track_id, event)),
                    Ok(None) => break,
                    Err(error) => {
                        track.lifecycle = AlignmentTrackLifecycle::Failed(error.to_string());
                        if Some(track_id) == primary_id {
                            crate::app::shell_state::report_error_to(
                                &mut self.shell.notifications,
                                &mut self.data_status,
                                error.to_string(),
                            );
                        }
                        break;
                    }
                }
            }
        }
        if events.is_empty() {
            return;
        }

        let mut opened_primary_dictionary = None;
        let mut source_opened = false;
        let mut rebuild_display = false;
        let mut prepare_frame = false;
        let mut reference_panels_to_request = BTreeSet::new();
        let mut highlight_panels_to_synchronize = BTreeSet::new();
        let mut ownerless_progress_frames = Vec::new();
        for (track_id, event) in events {
            let Some(track_index) = self
                .alignment_tracks
                .iter()
                .position(|track| track.config.id == track_id)
            else {
                continue;
            };
            match event {
                AlignmentSceneEngineEvent::SourceOpened { metadata } => {
                    let dictionary = metadata.dictionary.clone();
                    let track = &mut self.alignment_tracks[track_index];
                    track.lifecycle = AlignmentTrackLifecycle::Ready;
                    track.dictionary = Some(dictionary.clone());
                    track.index_overview = metadata.index_overview.clone();
                    track.scene_metadata = Some(metadata);
                    let panel_ids = track.projections.keys().copied().collect::<Vec<_>>();
                    let track_id = track.config.id;
                    for panel_id in panel_ids {
                        let token = self.take_alignment_scene_request_token();
                        let projection =
                            self.alignment_tracks[track_index].projection_mut(panel_id);
                        projection.navigation.bind_identity(panel_id, track_id);
                        projection.navigation.begin(
                            projection.clock.current().get(),
                            token,
                            "alignment source opened",
                        );
                    }
                    source_opened = true;
                    rebuild_display = true;
                    if Some(track_id) == primary_id {
                        opened_primary_dictionary = Some(dictionary);
                    }
                }
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner,
                    request_token,
                    cause,
                    published_scene,
                    frame,
                    work,
                    telemetry,
                    update,
                } => {
                    let frame = alignment_scene_frame_for_ui(frame);
                    let panel_id = self.alignment_tracks[track_index]
                        .projections
                        .iter()
                        .find_map(|(panel_id, projection)| {
                            (projection.scene_owner == Some(owner)).then_some(*panel_id)
                        });
                    let Some(panel_id) = panel_id else {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    };
                    if self.manual_diagnostic_trace_is_recording()
                        && cause == AlignmentSceneFrameCause::SourceSettlement
                    {
                        self.record_manual_diagnostic_event(
                            "alignment_worker",
                            "source_settlement_without_scene_publication",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "panel_id": panel_id.get(),
                                "owner": owner.get(),
                                "request_token": request_token.get(),
                                "camera_sequence": frame.camera_sequence,
                                "camera": format!("{:?}", frame.camera.interval()),
                                "reference_requirement": format!("{:?}", frame.reference_requirement),
                                "individual_reads": format!("{:?}", frame.individual_reads),
                                "visible_complete": frame.visible_complete(),
                                "scene_contracts_current": frame.scene_contracts_current,
                                "reference_products_current": frame.reference_products_current,
                                "source_pending": work.source_pending,
                                "presentation_pending": work.presentation_pending,
                            }),
                        );
                    }
                    if self.manual_diagnostic_trace_is_recording()
                        && let Some(capacity) = update.and_then(|update| update.capacity_plan)
                    {
                        self.record_manual_diagnostic_event(
                            "alignment_cache",
                            "foreground_capacity_plan",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "panel_id": panel_id.get(),
                                "owner": owner.get(),
                                "camera_sequence": frame.camera_sequence,
                                "budget": capacity.budget,
                                "charged": capacity.charged,
                                "pinned": capacity.pinned,
                                "reserved": capacity.reserved,
                                "foreground_reserve_bytes": capacity.foreground_reserve_bytes,
                                "low_water_target": capacity.low_water_target,
                                "return_cache_bytes": capacity.return_cache_bytes,
                                "disposable_speculation_bytes": capacity.disposable_speculation_bytes,
                                "proactive_reclaim_target": update.and_then(|update| update.proactive_reclaim_target),
                            }),
                        );
                    }
                    if self.alignment_tracks[track_index]
                        .projection(panel_id)
                        .is_some_and(|projection| projection.index_only_overview)
                    {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    }
                    let track = &mut self.alignment_tracks[track_index];
                    let projection = track.projection_mut(panel_id);
                    if !projection
                        .navigation
                        .worker_frame_is_admissible(request_token, frame.camera)
                    {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    }
                    let monotonic = projection
                        .scene_frame
                        .as_ref()
                        .is_none_or(|current| frame.camera_sequence >= current.camera_sequence);
                    if !monotonic {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    }
                    let row_activity_changed = projection
                        .scene_frame
                        .as_ref()
                        .and_then(|current| current.row_activity.as_deref())
                        != frame.row_activity.as_deref();
                    let previous_highlight_authority = projection
                        .scene_frame
                        .as_ref()
                        .and_then(|current| current.snapshot.as_ref())
                        .map(|snapshot| {
                            (
                                snapshot.reads().evidence().revision(),
                                snapshot.prepared_interval(),
                            )
                        });
                    let next_highlight_authority = frame.snapshot.as_ref().map(|snapshot| {
                        (
                            snapshot.reads().evidence().revision(),
                            snapshot.prepared_interval(),
                        )
                    });
                    if previous_highlight_authority != next_highlight_authority {
                        highlight_panels_to_synchronize.insert(panel_id);
                        rebuild_display = true;
                        prepare_frame = true;
                    }
                    let camera_refusal_is_terminal = frame.individual_reads_refused_for_camera();
                    projection.scene_frame = Some(frame.clone());
                    projection.scene_frame_request_token = Some(request_token);
                    projection.scene_telemetry = telemetry;
                    let current_target =
                        projection.navigation.desired_camera() == Some(frame.camera);
                    if current_target {
                        projection.unavailable = None;
                        projection.index_only_overview = false;
                        projection.navigation.observe_frame_products(&frame);
                        let reference_blocks_core_scene = projection
                            .scene_contract
                            .as_ref()
                            .is_some_and(|contract| contract.view_filter.requires_reference());
                        let settlement =
                            alignment_scene_frame_settlement(&frame, reference_blocks_core_scene);
                        if settlement != AlignmentSceneFrameSettlement::WaitingForScene {
                            projection.scene_pressure_retry = None;
                        }
                        match settlement {
                            AlignmentSceneFrameSettlement::Settled => {
                                // A later terminal frame in this same mailbox drain supersedes an
                                // earlier progress frame which may have queued a FASTA request.
                                reference_panels_to_request.remove(&panel_id);
                                projection.finish_checking();
                                if projection.navigation.phase()
                                    != AlignmentNavigationPhase::Committed
                                {
                                    projection.navigation.transition(
                                        AlignmentNavigationPhase::CandidateReady,
                                        "complete semantic scene is ready for atomic GPU publication",
                                    );
                                }
                            }
                            AlignmentSceneFrameSettlement::SettledWithReferencePending => {
                                // The core scene is complete and can commit now. FASTA-backed
                                // substitutions continue as an optional product upgrade under the
                                // same request token; they do not own alignment navigation.
                                reference_panels_to_request.insert(panel_id);
                                projection.finish_checking();
                                if projection.navigation.phase()
                                    != AlignmentNavigationPhase::Committed
                                {
                                    projection.navigation.transition(
                                        AlignmentNavigationPhase::CandidateReady,
                                        "reference-independent scene is ready; substitution interpretation remains pending",
                                    );
                                }
                            }
                            AlignmentSceneFrameSettlement::WaitingForReference => {
                                // A reference-backed view filter cannot produce a truthful row
                                // atlas or semantic scene until the exact FASTA authority arrives.
                                // Transfer navigation ownership away from the now-idle alignment
                                // source and queue the panel's current reference requirement.
                                reference_panels_to_request.insert(panel_id);
                                if projection.navigation.phase()
                                    != AlignmentNavigationPhase::WaitingForReference
                                {
                                    projection.navigation.transition(
                                        AlignmentNavigationPhase::WaitingForReference,
                                        "reference authority is required to evaluate the alignment view filter",
                                    );
                                }
                            }
                            AlignmentSceneFrameSettlement::WaitingForScene => {
                                if projection.navigation.phase()
                                    != AlignmentNavigationPhase::Running
                                {
                                    projection.navigation.transition(
                                        AlignmentNavigationPhase::Running,
                                        "source or row-presentation work is active",
                                    );
                                }
                                ownerless_progress_frames.retain(
                                    |(candidate_track, candidate_panel, _, _)| {
                                        *candidate_track != track_id || *candidate_panel != panel_id
                                    },
                                );
                                if !work.source_pending && !work.presentation_pending {
                                    ownerless_progress_frames.push((
                                        track_id,
                                        panel_id,
                                        request_token,
                                        frame.camera,
                                    ));
                                }
                            }
                        }
                    }
                    if Some(track_id) == primary_id && panel_id == active_panel_id && current_target
                    {
                        match frame.individual_reads {
                            AlignmentIndividualReadCapability::Complete { .. } => {
                                let records = frame.snapshot.as_ref().map_or(0, |snapshot| {
                                    snapshot.reads().evidence().record_count()
                                });
                                self.data_status = match frame.vertical_residency {
                                    AlignmentVerticalResidency::Preparing {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · preparing instant vertical browsing ({prepared_rows}/{total_rows} rows)"
                                    ),
                                    AlignmentVerticalResidency::Complete { total_rows } => {
                                        format!(
                                            "{records} complete alignments · all {total_rows} rows prepared"
                                        )
                                    }
                                    AlignmentVerticalResidency::BudgetLimited {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · {prepared_rows}/{total_rows} rows resident (cache budget reached)"
                                    ),
                                    AlignmentVerticalResidency::Unavailable {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · {prepared_rows}/{total_rows} rows prepared (instant deep scrolling unavailable)"
                                    ),
                                    AlignmentVerticalResidency::ViewportBounded => {
                                        format!("{records} complete alignments")
                                    }
                                };
                            }
                            AlignmentIndividualReadCapability::Pending => {
                                "Preparing surrounding alignment scene…"
                                    .clone_into(&mut self.data_status);
                            }
                            AlignmentIndividualReadCapability::Refused {
                                complete_read_limit,
                                eligible_records,
                                ..
                            } => {
                                self.data_status = format!(
                                    "{eligible_records} eligible alignments exceed the Complete read limit of {complete_read_limit}"
                                );
                            }
                        }
                    }
                    rebuild_display |= camera_refusal_is_terminal
                        || row_activity_changed
                        || (published_scene.is_some()
                            && cause != AlignmentSceneFrameCause::VerticalResidency);
                    // Optional vertical warm-up now populates independently keyed semantic pages
                    // without publishing a scene. Its progress/status event must not recapture or
                    // replace the foreground GPU presentation on every background row block.
                    prepare_frame |= cause != AlignmentSceneFrameCause::VerticalResidency
                        || published_scene.is_some()
                        || row_activity_changed;
                }
                AlignmentSceneEngineEvent::IndividualReadsRefused {
                    owner,
                    request_token,
                    target,
                    complete_read_limit,
                    eligible_records,
                    frame,
                    telemetry,
                } => {
                    let frame = alignment_scene_frame_for_ui(frame);
                    let panel_id = self.alignment_tracks[track_index]
                        .projections
                        .iter()
                        .find_map(|(panel_id, projection)| {
                            (projection.scene_owner == Some(owner)).then_some(*panel_id)
                        });
                    let Some(panel_id) = panel_id else {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    };
                    if self.alignment_tracks[track_index]
                        .projection(panel_id)
                        .is_some_and(|projection| projection.index_only_overview)
                    {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    }
                    let track = &mut self.alignment_tracks[track_index];
                    let projection = track.projection_mut(panel_id);
                    if !alignment_scene_refusal_is_current(
                        projection.navigation.request_token(),
                        request_token,
                        projection.navigation.desired_camera(),
                        frame.camera,
                        target,
                    ) {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    }
                    let monotonic = projection
                        .scene_frame
                        .as_ref()
                        .is_none_or(|current| frame.camera_sequence >= current.camera_sequence);
                    if !monotonic {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    }
                    projection.scene_frame = Some(frame.clone());
                    projection.scene_frame_request_token = Some(request_token);
                    projection.scene_telemetry = telemetry;
                    projection.scene_pressure_retry = None;
                    if projection.navigation.desired_camera() == Some(frame.camera) {
                        reference_panels_to_request.remove(&panel_id);
                        projection.finish_checking();
                        projection.navigation.transition(
                            AlignmentNavigationPhase::CandidateReady,
                            "complete source count produced a terminal individual-read refusal",
                        );
                    }
                    if Some(track_id) == primary_id && panel_id == active_panel_id {
                        self.data_status = format!(
                            "{eligible_records} eligible alignments exceed the Complete read limit of {complete_read_limit}"
                        );
                    }
                    rebuild_display = true;
                    prepare_frame = true;
                }
                AlignmentSceneEngineEvent::ResourcePressure {
                    owner,
                    request_token,
                    target,
                    operation,
                    intent,
                    pressure,
                    disposition,
                } => {
                    let panel_id = self.alignment_tracks[track_index]
                        .projections
                        .iter()
                        .find_map(|(panel_id, projection)| {
                            (projection.scene_owner == Some(owner)).then_some(*panel_id)
                        });
                    let Some(panel_id) = panel_id else {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    };
                    let current = self.alignment_tracks[track_index]
                        .projection(panel_id)
                        .is_some_and(|projection| {
                            !projection.index_only_overview
                                && alignment_scene_pressure_is_current(
                                    projection.navigation.request_token(),
                                    request_token,
                                    projection.navigation.desired_camera(),
                                    target,
                                )
                        });
                    if !current {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    }

                    // Source read-ahead is optional once the current frame is complete. It yields
                    // at its own checkpoint and must not displace a useful GPU scene or reopen the
                    // foreground navigation transaction.
                    if !alignment_scene_pressure_requires_resume(operation, intent) {
                        continue;
                    }

                    if alignment_scene_pressure_preserves_visible_epoch(
                        operation,
                        intent,
                        disposition,
                    ) {
                        let projection =
                            self.alignment_tracks[track_index].projection_mut(panel_id);
                        projection.scene_pressure_retry = None;
                        if Some(track_id) == primary_id && panel_id == active_panel_id {
                            let records = projection
                                .scene_frame
                                .as_ref()
                                .and_then(|frame| frame.snapshot.as_ref())
                                .map_or(0, |snapshot| snapshot.reads().evidence().record_count());
                            self.data_status = projection.scene_frame.as_ref().map_or_else(
                                || {
                                    "Complete alignments remain available · instant deep scrolling is limited by the cache budget"
                                        .to_owned()
                                },
                                |frame| match frame.vertical_residency {
                                    AlignmentVerticalResidency::Preparing {
                                        prepared_rows,
                                        total_rows,
                                    }
                                    | AlignmentVerticalResidency::BudgetLimited {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · {prepared_rows}/{total_rows} rows resident (cache budget reached)"
                                    ),
                                    AlignmentVerticalResidency::Unavailable {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · {prepared_rows}/{total_rows} rows prepared (instant deep scrolling unavailable)"
                                    ),
                                    AlignmentVerticalResidency::Complete { total_rows } => format!(
                                        "{records} complete alignments · all {total_rows} rows prepared"
                                    ),
                                    AlignmentVerticalResidency::ViewportBounded => format!(
                                        "{records} complete alignments · instant deep scrolling is limited by the cache budget"
                                    ),
                                },
                            );
                        }
                        continue;
                    }

                    if disposition == AlignmentSceneResourcePressureDisposition::Infeasible {
                        let message = format!(
                            "Alignment scene requires more cache capacity than the configured limit: {pressure}"
                        );
                        let projection =
                            self.alignment_tracks[track_index].projection_mut(panel_id);
                        projection.scene_pressure_retry = None;
                        projection.settle_unavailable(message.clone());
                        if Some(track_id) == primary_id && panel_id == active_panel_id {
                            self.report_warning(message);
                        }
                        rebuild_display = true;
                        continue;
                    }

                    let snapshot_before = match self.alignment_scene_ledger.progress() {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            self.data_status =
                                format!("Could not inspect alignment cache capacity: {error}");
                            continue;
                        }
                    };
                    let answers_existing_retry = self.alignment_tracks[track_index]
                        .projection(panel_id)
                        .and_then(|projection| projection.scene_pressure_retry)
                        .is_some_and(|retry| retry.same_request(request_token, target));
                    let post_relief = if answers_existing_retry {
                        // This is the typed answer to the queued retry. Revalidate its newest exact
                        // obstruction even if occupancy changed while the request was in flight.
                        snapshot_before
                    } else {
                        match self.yield_alignment_gpu_speculation_for_source_pressure(pressure) {
                            Ok(_) => {}
                            Err(error) => {
                                self.data_status =
                                    format!("Could not yield optional GPU residency: {error}");
                                continue;
                            }
                        };
                        match self.alignment_scene_ledger.progress() {
                            Ok(snapshot) => snapshot,
                            Err(error) => {
                                self.data_status =
                                    format!("Could not inspect alignment cache capacity: {error}");
                                continue;
                            }
                        }
                    };
                    let retry_epoch = post_relief.progress_epoch();
                    let pressure_owns_navigation = alignment_scene_pressure_owns_navigation(
                        self.alignment_tracks[track_index]
                            .projection(panel_id)
                            .map_or(AlignmentNavigationPhase::Dormant, |projection| {
                                projection.navigation.phase()
                            }),
                    );
                    if let AlignmentScenePressureRetryPoll::Waiting(waiting) =
                        alignment_scene_pressure_after_relief(
                            request_token,
                            target,
                            pressure,
                            &post_relief,
                        )
                    {
                        // A first pressure edge is not itself permission for a guaranteed-failing
                        // retry. Record the exact post-yield blocker and wait for relevant ledger
                        // progress; a later pressure answer replaces it through the same path.
                        self.alignment_tracks[track_index]
                            .projection_mut(panel_id)
                            .scene_pressure_retry = Some(waiting);
                        if pressure_owns_navigation {
                            self.alignment_tracks[track_index]
                                .projection_mut(panel_id)
                                .navigation
                                .transition(
                                    AlignmentNavigationPhase::WaitingForSourcePressure,
                                    "source allocation is paused on an exact ledger blocker",
                                );
                            if Some(track_id) == primary_id && panel_id == active_panel_id {
                                "Waiting for shared alignment cache capacity…"
                                    .clone_into(&mut self.data_status);
                            }
                        }
                        continue;
                    }
                    let retry_result = self.alignment_tracks[track_index]
                        .scene_engine
                        .as_ref()
                        .map(|engine| {
                            engine.try_retry_resource_pressure(owner, request_token, target)
                        });
                    let retry_queued = matches!(retry_result, Some(Ok(())));
                    self.alignment_tracks[track_index]
                        .projection_mut(panel_id)
                        .scene_pressure_retry = Some(AlignmentScenePressureRetry {
                        request_token,
                        target,
                        pressure,
                        ledger_epoch: retry_epoch,
                        in_flight: retry_queued,
                    });
                    match retry_result {
                        Some(Ok(())) => {
                            if pressure_owns_navigation {
                                self.alignment_tracks[track_index]
                                    .projection_mut(panel_id)
                                    .navigation
                                    .transition(
                                        AlignmentNavigationPhase::Running,
                                        "source pressure retry was accepted",
                                    );
                                if Some(track_id) == primary_id && panel_id == active_panel_id {
                                    "Updating alignment scene…".clone_into(&mut self.data_status);
                                }
                            }
                        }
                        Some(Err(EngineQueueError::Disconnected)) | None => {
                            self.alignment_tracks[track_index]
                                .projection_mut(panel_id)
                                .settle_failed("alignment source mailbox disconnected");
                            self.alignment_tracks[track_index].lifecycle =
                                AlignmentTrackLifecycle::Failed(
                                    EngineQueueError::Disconnected.to_string(),
                                );
                        }
                        Some(Err(EngineQueueError::Full)) => {
                            // The retry API is replaceable and currently cannot report Full, but
                            // keep the state paused if that mailbox contract ever changes.
                        }
                    }
                }
                AlignmentSceneEngineEvent::BudgetPressure {
                    requested_budget,
                    clear_before_apply,
                    current_policy_revision,
                    current_budget,
                    pressure,
                } => {
                    let desired_budget = self.alignment_scene_budget.budget_bytes;
                    let submitted_budget =
                        self.alignment_tracks[track_index].scene_budget_bytes.get();
                    if !alignment_scene_budget_event_is_current(
                        desired_budget,
                        submitted_budget,
                        requested_budget,
                    ) {
                        // A later settings/rebalance command is already ordered behind this
                        // response. Never revive the stale value when the old GPU transition
                        // eventually settles.
                        continue;
                    }
                    let snapshot = match self.alignment_scene_ledger.progress() {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            self.data_status =
                                format!("Could not inspect alignment cache capacity: {error}");
                            continue;
                        }
                    };
                    self.alignment_tracks[track_index].scene_budget_retry =
                        Some(AlignmentSceneBudgetRetry::after_pressure_event(
                            requested_budget,
                            clear_before_apply,
                            current_policy_revision,
                            current_budget,
                            pressure,
                            &snapshot,
                        ));
                }
                AlignmentSceneEngineEvent::BudgetReduced { pressure } => {
                    if self.alignment_tracks[track_index]
                        .scene_budget_retry
                        .is_some_and(|retry| retry.requested_budget == pressure.budget)
                    {
                        self.alignment_tracks[track_index].scene_budget_retry = None;
                    }
                }
                AlignmentSceneEngineEvent::InactiveEvicted {
                    request_id,
                    eviction,
                } => {
                    if self.manual_diagnostic_trace_is_recording() {
                        self.record_manual_diagnostic_event(
                            "alignment_cache",
                            "reclaim_completed",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "request_id": request_id,
                                "target_charged": eviction.target_charged,
                                "charged_before": eviction.charged_before,
                                "charged_after": eviction.charged,
                                "pinned_after": eviction.pinned,
                                "released_bytes": eviction.released,
                                "relinquished_cache_entries": eviction.relinquished_cache_entries,
                                "quantum_entries": eviction.quantum_entries,
                                "quanta": eviction.quanta,
                                "elapsed_nanoseconds": eviction.elapsed_nanoseconds,
                                "reclaim_complete": eviction.reclaim_complete,
                                "remaining_bytes": eviction.remaining,
                                "semantic_candidates_inspected": eviction.presentation.candidates_inspected,
                                "semantic_speculation_entries": eviction.presentation.residency.disposable_speculation_entries,
                                "semantic_speculation_bytes": eviction.presentation.residency.disposable_speculation_bytes,
                                "semantic_active_cold_entries": eviction.presentation.residency.active_locus_cold_entries,
                                "semantic_active_cold_bytes": eviction.presentation.residency.active_locus_cold_bytes,
                                "semantic_active_near_entries": eviction.presentation.residency.active_near_buffer_entries,
                                "semantic_active_near_bytes": eviction.presentation.residency.active_near_buffer_bytes,
                                "semantic_foreground_entries": eviction.presentation.residency.foreground_visible_entries,
                                "semantic_foreground_bytes": eviction.presentation.residency.foreground_visible_bytes,
                                "semantic_return_cache_entries": eviction.presentation.residency.recent_locus_return_cache_entries,
                                "semantic_return_cache_bytes": eviction.presentation.residency.recent_locus_return_cache_bytes,
                                "semantic_read_pages_evicted": eviction.presentation.evicted_read_pages,
                                "semantic_summary_pages_evicted": eviction.presentation.evicted_summary_pages,
                                "evidence_candidates_inspected": eviction.evidence.candidates_inspected,
                                "evidence_active_near_entries": eviction.evidence.residency.active_near_buffer_entries,
                                "evidence_active_near_bytes": eviction.evidence.residency.active_near_buffer_bytes,
                                "evidence_return_cache_entries": eviction.evidence.residency.recent_locus_return_cache_entries,
                                "evidence_return_cache_bytes": eviction.evidence.residency.recent_locus_return_cache_bytes,
                                "evidence_acquisitions_evicted": eviction.evidence.evicted_acquisitions,
                                "evidence_chunks_evicted": eviction.evidence.evicted_chunks,
                                "exact_summaries_evicted": eviction.evidence.evicted_exact_summaries,
                                "row_atlases_evicted": eviction.evidence.evicted_row_atlases,
                            }),
                        );
                    }
                    self.observe_alignment_capacity_reclaim(
                        track_id,
                        request_id,
                        eviction.released,
                        eviction.relinquished_cache_entries,
                    );
                }
                AlignmentSceneEngineEvent::InactiveEvictionProgress {
                    request_id,
                    eviction,
                } => {
                    if self.manual_diagnostic_trace_is_recording() {
                        self.record_manual_diagnostic_event(
                            "alignment_cache",
                            "reclaim_quantum",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "request_id": request_id,
                                "target_charged": eviction.target_charged,
                                "charged_before": eviction.charged_before,
                                "charged_after": eviction.charged,
                                "pinned_after": eviction.pinned,
                                "released_bytes": eviction.released,
                                "relinquished_cache_entries": eviction.relinquished_cache_entries,
                                "quantum_entries": eviction.quantum_entries,
                                "elapsed_nanoseconds": eviction.elapsed_nanoseconds,
                                "reclaim_complete": eviction.reclaim_complete,
                                "remaining_bytes": eviction.remaining,
                            }),
                        );
                    }
                }
                AlignmentSceneEngineEvent::InactiveEvictionSuperseded {
                    request_id,
                    target_charged,
                    released_bytes,
                    relinquished_cache_entries,
                    reason,
                } => {
                    if self.manual_diagnostic_trace_is_recording() {
                        self.record_manual_diagnostic_event(
                            "alignment_cache",
                            "reclaim_superseded",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "request_id": request_id,
                                "target_charged": target_charged,
                                "released_bytes": released_bytes,
                                "relinquished_cache_entries": relinquished_cache_entries,
                                "reason": format!("{reason:?}"),
                            }),
                        );
                    }
                    self.observe_alignment_capacity_reclaim(
                        track_id,
                        request_id,
                        released_bytes,
                        relinquished_cache_entries,
                    );
                }
                AlignmentSceneEngineEvent::WorkerPhaseCompleted {
                    phase,
                    owner,
                    elapsed_nanoseconds,
                } => {
                    if self.manual_diagnostic_trace_is_recording() {
                        self.record_manual_diagnostic_event(
                            "alignment_worker",
                            "slow_phase_completed",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "owner": owner.map(SourceOwnerId::get),
                                "phase": format!("{phase:?}"),
                                "elapsed_nanoseconds": elapsed_nanoseconds,
                            }),
                        );
                    }
                }
                AlignmentSceneEngineEvent::Error {
                    owner,
                    request_token,
                    operation,
                    message,
                } => {
                    let panel_id = owner.and_then(|owner| {
                        self.alignment_tracks[track_index]
                            .projections
                            .iter()
                            .find_map(|(panel_id, projection)| {
                                (projection.scene_owner == Some(owner)).then_some(*panel_id)
                            })
                    });
                    if let Some(panel_id) = panel_id {
                        let projection =
                            self.alignment_tracks[track_index].projection_mut(panel_id);
                        if !alignment_scene_event_token_is_current(
                            projection.navigation.request_token(),
                            request_token,
                        ) {
                            self.navigation_stale_result_rejections =
                                self.navigation_stale_result_rejections.saturating_add(1);
                            continue;
                        }
                        projection.settle_failed(message.to_string());
                    } else if owner.is_some() {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    } else if matches!(
                        operation,
                        rgv_engine::AlignmentSceneEngineOperation::OpenSource
                    ) {
                        self.alignment_tracks[track_index].lifecycle =
                            AlignmentTrackLifecycle::Failed(message.to_string());
                        for projection in
                            self.alignment_tracks[track_index].projections.values_mut()
                        {
                            projection.settle_failed(message.to_string());
                        }
                    } else {
                        // Source pumping is shared by every lane and therefore has no owner in its
                        // error event. It is nevertheless terminal for each currently requested
                        // panel generation. Settle those generations explicitly so a truthful
                        // error cannot be hidden forever behind "Updating view…"; the next camera
                        // or contract request may retry using the last immutable scene.
                        for projection in
                            self.alignment_tracks[track_index].projections.values_mut()
                        {
                            projection.settle_failed(message.to_string());
                        }
                    }
                    if Some(track_id) == primary_id
                        && panel_id.is_none_or(|panel_id| panel_id == active_panel_id)
                    {
                        self.report_error(message.to_string());
                    }
                    rebuild_display = true;
                }
                AlignmentSceneEngineEvent::LaneInserted { .. }
                | AlignmentSceneEngineEvent::LaneRemoved { .. }
                | AlignmentSceneEngineEvent::PresentationPrewarmed { .. }
                | AlignmentSceneEngineEvent::SourceControlQueued => {}
            }
        }

        // Error and progress events can already coexist in one drained mailbox batch. Defer the
        // ownerless-worker invariant until every event has had a chance to settle the request;
        // otherwise an earlier progress frame can replace the real terminal error with a
        // synthetic liveness failure. A later frame for the same lane supersedes the candidate
        // above, and a terminal event makes it inadmissible here.
        for (track_id, panel_id, request_token, camera) in ownerless_progress_frames {
            let Some(track_index) = self
                .alignment_tracks
                .iter()
                .position(|track| track.config.id == track_id)
            else {
                continue;
            };
            let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
            if !projection
                .navigation
                .worker_frame_is_admissible(request_token, camera)
            {
                continue;
            }
            if projection
                .navigation
                .fail_if_source_owner_is_idle(false, false)
            {
                projection.finish_checking();
                projection.unavailable = Some(
                    "Internal liveness failure: the alignment worker became idle before completing this target"
                        .to_owned(),
                );
                if Some(track_id) == primary_id && panel_id == active_panel_id {
                    self.report_error("Internal liveness failure: the alignment worker became idle before completing this target");
                }
                rebuild_display = true;
            }
        }

        if !reference_panels_to_request.is_empty() {
            let restore = self.panels.active().id;
            for panel_id in reference_panels_to_request {
                self.panels.activate(panel_id);
                self.panels.active_mut().pending_queries.reference = true;
                self.dispatch_reference_query();
            }
            self.panels.activate(restore);
        }

        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for panel_id in panel_ids {
            let pending = self.alignment_tracks.iter().any(|track| {
                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
                    && track.projection(panel_id).is_some_and(|projection| {
                        !projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    })
            });
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel.pending_queries.alignment = pending;
            }
        }

        if let Some(dictionary) = opened_primary_dictionary {
            let reference_incompatibility = self.alignment_reference_incompatibility(&dictionary);
            if let Some(source) = self.primary_alignment_source().cloned() {
                self.settings.remember_alignment(&source);
                self.persist_settings();
            }
            let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
            let mut imported_feature_search = None;
            for id in ids {
                let (requested, pixel_width) = self
                    .panels
                    .get(id)
                    .map(|panel| (panel.requested_locus.clone(), panel.viewport.pixel_width()))
                    .expect("panel id came from the panel collection");
                match self
                    .resolve_panel_requested_locus(&dictionary, requested.as_deref())
                    .and_then(|locus| Ok((Viewport::new(locus.interval, pixel_width)?, locus)))
                {
                    Ok((viewport, locus)) => {
                        let RequestedLocus {
                            interval,
                            contig_name: name,
                            contig_length: length,
                            feature_search,
                        } = locus;
                        imported_feature_search = imported_feature_search.or(feature_search);
                        let panel = self
                            .panels
                            .get_mut(id)
                            .expect("panel id came from the panel collection");
                        panel.viewport = viewport;
                        panel.contig_name = name;
                        panel.contig_length = length;
                        panel.reset_history(interval);
                        panel.vertical_scroll_points = 0.0;
                        panel.vertical_content_origin_points = 0.0;
                        panel.latent_vertical_scroll_fraction = None;
                    }
                    Err(error) if id == active_panel_id => self.report_error(error.to_string()),
                    Err(_) => {}
                }
            }
            self.finish_imported_locus(imported_feature_search);
            let default_annotation_count = self.assembly.default_annotation_count;
            let (assembly, cytoband_failure) = load_assembly_reporting(
                &self.assembly_name,
                dictionary.clone(),
                self.cytoband_path.as_deref(),
            );
            self.assembly = assembly;
            if let Some(failure) = cytoband_failure {
                self.report_error(failure);
            }
            self.assembly.default_annotation_count = default_annotation_count;
            self.assembly.reference_contig_count = self
                .reference_dictionary
                .as_ref()
                .map_or(dictionary.len(), ReferenceDictionary::len);
            self.assembly.identity.clone_from(&self.assembly_identity);
            if let Some(reason) = reference_incompatibility {
                for panel in self.panels.iter_mut() {
                    panel.reference_slice = None;
                    panel.reference_slice_revision = None;
                    panel.reference_slice_generation = None;
                    panel.reference_request_revision = None;
                    panel.reference_request_interval = None;
                    panel.reference_installs_queued.clear();
                }
                self.data_status = format!(
                    "Reference incompatible with alignment: {reason}. Reference-dependent bases and substitutions are disabled"
                );
            }
        }
        if source_opened {
            // Rebuild first so dispatch can derive the read-pane clip from committed layout.
            self.rebuild_all_panel_displays();
            self.request_all_panel_queries();
            rebuild_display = false;
            prepare_frame = true;
        }
        for panel_id in highlight_panels_to_synchronize {
            self.synchronize_alignment_scene_interactions(panel_id);
        }
        if rebuild_display {
            self.rebuild_all_panel_displays();
            prepare_frame = true;
        }
        if prepare_frame {
            self.prepare_alignment_scene_frame();
        }
        self.window.request_redraw();
    }

    #[allow(clippy::too_many_lines)]
    /// Logs a per-track warning the first time a text track reports skipped malformed lines.
    fn report_track_source_diagnostics(&mut self) {
        let Some(engine) = &self.track_engine else {
            return;
        };
        for (track_id, diagnostics) in engine.source_diagnostics() {
            let name = self
                .tracks
                .tracks()
                .iter()
                .find(|track| track.id == track_id)
                .map_or_else(
                    || format!("track {}", track_id.get()),
                    |track| track.name.clone(),
                );
            if let Some(message) = self.shell.skipped_lines.observe(
                track_id,
                &name,
                diagnostics.malformed_lines,
                diagnostics.first_malformed_line.as_ref(),
            ) {
                tracing::warn!(track_id = track_id.get(), %message, "track source skipped lines");
                self.shell.notifications.push(
                    app::shell_state::NotificationSeverity::Warning,
                    Some(track_id),
                    message,
                );
                self.window.request_redraw();
            }
        }
    }

    pub(crate) fn poll_track_events(&mut self) {
        let mut events = Vec::new();
        if let Some(engine) = &self.track_engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        self.report_error(error.to_string());
                        break;
                    }
                }
            }
        }
        let had_events = !events.is_empty();
        if had_events {
            // Ready / TrackAdded / TracksAdded / LaneCompleted may each have met malformed
            // lines; the reporter logs each track once however many events arrive.
            self.report_track_source_diagnostics();
        }
        for event in events {
            let stale_query_result = match &event {
                TrackEngineEvent::QueryCompleted { request, .. }
                | TrackEngineEvent::LaneCompleted { request, .. }
                | TrackEngineEvent::Error {
                    request: Some(request),
                    ..
                } => self
                    .panels
                    .get(request.panel_id())
                    .is_none_or(|panel| !panel.accepts(*request)),
                _ => false,
            };
            if stale_query_result {
                self.navigation_stale_result_rejections =
                    self.navigation_stale_result_rejections.saturating_add(1);
            }
            if let TrackEngineEvent::Error {
                track: Some(track_id),
                message,
                ..
            } = &event
                && self.automation_auxiliary_open_state.contains_key(track_id)
            {
                tracing::warn!(
                    track_id = track_id.get(),
                    error = %message,
                    "auxiliary track failed to open"
                );
                self.automation_auxiliary_open_state.insert(
                    *track_id,
                    AutomationAuxiliaryOpenState::Failed(message.clone()),
                );
            }
            match event {
                TrackEngineEvent::Ready { opened, failed } => {
                    tracing::info!(opened, failed, "auxiliary tracks opened");
                    let pending = self
                        .automation_auxiliary_open_state
                        .iter()
                        .filter_map(|(id, state)| {
                            (state == &AutomationAuxiliaryOpenState::Pending).then_some(*id)
                        })
                        .collect::<Vec<_>>();
                    if pending.len() == 1 {
                        let state = if failed == 0 && opened > 0 {
                            AutomationAuxiliaryOpenState::Ready
                        } else {
                            AutomationAuxiliaryOpenState::Failed(
                                "The auxiliary source could not be opened".to_owned(),
                            )
                        };
                        self.automation_auxiliary_open_state
                            .insert(pending[0], state);
                    }
                    if !self.synthetic {
                        self.request_all_panel_queries();
                    }
                }
                TrackEngineEvent::TrackAdded { id } => {
                    if self.automation_auxiliary_open_state.contains_key(&id) {
                        self.automation_auxiliary_open_state
                            .insert(id, AutomationAuxiliaryOpenState::Ready);
                    }
                    self.request_all_panel_queries();
                }
                TrackEngineEvent::TracksAdded { ids } => {
                    for id in ids {
                        if self.automation_auxiliary_open_state.contains_key(&id) {
                            self.automation_auxiliary_open_state
                                .insert(id, AutomationAuxiliaryOpenState::Ready);
                        }
                    }
                    self.request_all_panel_queries();
                }
                TrackEngineEvent::TracksRemoved { ids } => {
                    let removed = ids.into_iter().collect::<HashSet<_>>();
                    for panel in self.panels.iter_mut() {
                        clear_removed_auxiliary_panel_state(panel, &removed);
                    }
                    self.annotation_search_results
                        .hits
                        .retain(|hit| !removed.contains(&hit.track_id));
                    self.mark_display_dirty("explicit display invalidation");
                }
                TrackEngineEvent::TdfChannelsDiscovered {
                    id,
                    track_names,
                    selected_index,
                } => {
                    if let Err(error) = self.expand_tdf_channels(id, &track_names, selected_index) {
                        self.report_error(error.to_string());
                    }
                }
                TrackEngineEvent::LaneCompleted {
                    request,
                    interval,
                    track,
                } => {
                    let is_active = request.panel_id() == self.panels.active().id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                        && panel.track_request_interval == Some(interval)
                    {
                        let track_id = track.id;
                        panel.track_query_error_generations.remove(&track.id);
                        panel.track_snapshot_intervals.insert(track.id, interval);
                        panel
                            .track_snapshot_generations
                            .insert(track.id, request.generation().get());
                        // Selections index into the lane's result. Re-resolve them against
                        // the refreshed result by record identity instead of clearing them.
                        let position = panel
                            .track_snapshots
                            .iter()
                            .position(|snapshot| snapshot.id == track_id);
                        let (previous, position) = match position {
                            Some(position) => (
                                Some(std::mem::replace(
                                    &mut panel.track_snapshots[position],
                                    track,
                                )),
                                position,
                            ),
                            None => {
                                panel.track_snapshots.push(track);
                                (None, panel.track_snapshots.len() - 1)
                            }
                        };
                        let selections =
                            crate::inspection::stable_selection::reresolve_auxiliary_selections(
                                crate::inspection::stable_selection::AuxiliarySelections {
                                    feature: panel.selected_feature,
                                    variant: panel.selected_variant,
                                    gwas: panel.selected_gwas,
                                    quantitative: panel.selected_quantitative,
                                    copy_number: panel.selected_copy_number,
                                    mutation: panel.selected_mutation,
                                    interaction: panel.selected_interaction,
                                },
                                previous.as_ref(),
                                &panel.track_snapshots[position],
                            );
                        panel.selected_feature = selections.feature;
                        panel.selected_variant = selections.variant;
                        panel.selected_gwas = selections.gwas;
                        panel.selected_quantitative = selections.quantitative;
                        panel.selected_copy_number = selections.copy_number;
                        panel.selected_mutation = selections.mutation;
                        panel.selected_interaction = selections.interaction;
                        self.mark_display_dirty("explicit display invalidation");
                        if is_active {
                            self.rebuild_genomic_display();
                        }
                    }
                }
                TrackEngineEvent::QueryCompleted {
                    request,
                    interval,
                    completed: _,
                } => {
                    let is_active = request.panel_id() == self.panels.active().id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                        && panel.track_request_interval == Some(interval)
                    {
                        panel.track_request_interval = None;
                        panel.track_settled_generation = Some(request.generation().get());
                        self.mark_display_dirty("explicit display invalidation");
                        if is_active {
                            self.rebuild_genomic_display();
                        }
                    }
                }
                TrackEngineEvent::SearchCompleted {
                    generation,
                    mut results,
                } if generation == self.annotation_search_generation => {
                    let pending = self
                        .pending_annotation_navigation
                        .take()
                        .filter(|query| query.eq_ignore_ascii_case(results.query.trim()));
                    self.merge_landmark_search_results(&results.query.clone(), &mut results);
                    self.annotation_search_results = results;
                    if let Some(query) = pending {
                        self.submit_annotation_search_results(&query);
                    }
                    self.mark_display_dirty("explicit display invalidation");
                }
                TrackEngineEvent::AdjacentFeatureCompleted { generation, hit }
                    if generation == self.annotation_navigation_generation =>
                {
                    if let Some(hit) = hit {
                        self.navigate_to_annotation_hit(&hit);
                    } else {
                        "No further annotation feature on this contig"
                            .clone_into(&mut self.data_status);
                    }
                }
                TrackEngineEvent::Error {
                    request,
                    track,
                    message,
                } => {
                    if let Some(request) = request {
                        let is_active = request.panel_id() == self.panels.active().id;
                        let accepted = self
                            .panels
                            .get(request.panel_id())
                            .is_some_and(|panel| panel.accepts(request));
                        if accepted {
                            if let Some(panel) = self.panels.get_mut(request.panel_id()) {
                                if let Some(track_id) = track {
                                    panel
                                        .track_query_error_generations
                                        .insert(track_id, request.generation().get());
                                }
                            }
                            self.mark_display_dirty("explicit display invalidation");
                            // Logged for every panel; only the active panel's failure also
                            // takes the status line.
                            self.log_track_error(track, &message);
                            if is_active {
                                self.data_status = message;
                            }
                        }
                    } else {
                        self.log_track_error(track, &message);
                        self.data_status = message;
                    }
                }
                TrackEngineEvent::SearchCompleted { .. }
                | TrackEngineEvent::AdjacentFeatureCompleted { .. } => {}
            }
        }
        if had_events {
            self.window.request_redraw();
        }
    }
}
