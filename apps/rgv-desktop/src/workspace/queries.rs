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
    desired_camera: Option<AlignmentCamera>,
    frame_camera: AlignmentCamera,
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
    desired_camera: Option<AlignmentCamera>,
    frame_camera: AlignmentCamera,
    refused_horizon: GenomicInterval,
) -> bool {
    alignment_scene_frame_is_current(desired_token, event_token, desired_camera, frame_camera)
        && refused_horizon.contains_interval(frame_camera.interval())
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
    fn mark_active_panel_navigation_queries_pending(&mut self) {
        let panel_id = self.panels.active().id;
        for track in &mut self.alignment_tracks {
            track.projection_mut(panel_id).pending_query =
                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_));
        }
        let alignment_pending = self.alignment_tracks.iter().any(|track| {
            track
                .projection(panel_id)
                .is_some_and(|projection| projection.pending_query)
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
        panel.reference_query_error_generation = None;
        panel.track_query_error_generation = None;
    }

    pub(crate) fn begin_viewport_navigation(
        &mut self,
        gesture_active: bool,
        hold_commit_until_end: bool,
    ) {
        if self
            .panels
            .active()
            .navigation_presentation
            .targets_viewport(self.panels.active().viewport)
        {
            return;
        }
        if self.synthetic
            || !(self.has_alignment_engine()
                || self.reference_engine.is_some()
                || self.track_engine.is_some())
        {
            self.rebuild_genomic_display();
            self.display_dirty = true;
            return;
        }

        let now = Instant::now();
        self.retain_active_read_selection_for_query_refresh();
        let panel_id = self.panels.active().id;
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
        let captured_source_revision = self.navigation_texture_revisions.get(&panel_id).copied();
        let panel = self.panels.active_mut();
        if matches!(
            previous_phase,
            NavigationPresentationPhase::Settled
                | NavigationPresentationPhase::ReadyToCommit
                | NavigationPresentationPhase::Committing
        ) {
            panel.navigation_query_schedule.begin_transaction();
        } else if previous_target_generation != generation {
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
            .navigation_presentation
            .require_captured_source_revision(captured_source_revision);
        panel.navigation_provisional_display = None;
        panel
            .navigation_query_schedule
            .update(target, now, gesture_active, hold_commit_until_end);
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
        self.try_build_complete_horizon_preview(panel_id, target);
    }

    pub(crate) fn panel_navigation_data_is_settled(&self, panel_id: PanelId) -> bool {
        let Some(panel) = self.panels.get(panel_id) else {
            return false;
        };
        if panel.navigation_query_schedule.pending.is_some()
            || panel.pending_queries.alignment
            || panel.pending_queries.reference
            || panel.pending_queries.tracks
        {
            return false;
        }
        let panel_generation = panel.generation_clock.current().get();
        let alignments_settled = self.panel_alignment_queries_are_settled(panel_id);
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
        let tracks_settled = self.track_engine.is_none()
            || panel.track_query_error_generation == Some(panel_generation)
            || (panel.track_settled_generation == Some(panel_generation)
                && panel.track_snapshot_interval == Some(panel.viewport.interval()));
        alignments_settled && reference_settled && tracks_settled
    }

    fn panel_alignment_queries_are_settled(&self, panel_id: PanelId) -> bool {
        self.alignment_tracks
            .iter()
            .all(|track| match track.lifecycle {
                AlignmentTrackLifecycle::Opening => false,
                AlignmentTrackLifecycle::Ready => track
                    .projection(panel_id)
                    .is_some_and(AlignmentPanelProjection::query_is_current),
                AlignmentTrackLifecycle::Failed(_) => true,
            })
    }

    fn update_navigation_readiness(&mut self) {
        let candidate_ids = self
            .panels
            .iter()
            .filter(|panel| {
                matches!(
                    panel.navigation_presentation.phase,
                    NavigationPresentationPhase::Preview
                        | NavigationPresentationPhase::CheckingWithoutPreview
                )
            })
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        let active_id = self.panels.active().id;
        let mut ready = Vec::new();
        for panel_id in candidate_ids {
            let alignment_queries_settled = self.panel_alignment_queries_are_settled(panel_id);
            self.panels.activate(panel_id);
            let row_layouts_settled = alignment_queries_settled && self.ensure_alignment_rows();
            let data_settled = self.panel_navigation_data_is_settled(panel_id);
            let panel = self
                .panels
                .get(panel_id)
                .expect("navigation candidate belongs to the panel workspace");
            if navigation_target_ready_for_commit(
                panel.navigation_presentation.phase,
                panel.navigation_query_schedule.hold_commit_until_end,
                data_settled,
                row_layouts_settled,
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
        self.display_dirty = true;
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
                panel.navigation_provisional_display = None;
                panel.navigation_query_schedule.record_commit(elapsed);
            }
        }
        committed
    }

    pub(crate) fn invalidate_navigation_preview_contract(&mut self, reason: &str) {
        self.navigation_contract_revision = self.navigation_contract_revision.saturating_add(1);
        for panel in self.panels.iter_mut() {
            panel
                .navigation_presentation
                .invalidate_preview(reason.to_owned());
            panel.navigation_provisional_display = None;
        }
        self.discard_navigation_preview_texture();
    }

    pub(crate) fn discard_navigation_preview_texture(&mut self) {
        self.navigation_texture_revisions.clear();
        self.navigation_preview_renderer.invalidate_texture();
    }

    /// Starts an atomic same-locus refresh after a query/presentation contract
    /// change. The old pixels are deliberately ineligible for transformation,
    /// so the panel uses the neutral checking state until all replacement data
    /// can commit in one frame.
    pub(crate) fn begin_all_panel_refresh_transactions(&mut self) {
        let now = Instant::now();
        let source_status = self.data_status.clone();
        for panel in self.panels.iter_mut() {
            panel.navigation_query_schedule.cancel();
            panel.navigation_provisional_display = None;
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
            let resolution = AlignmentQueryResolution::for_viewport(self.panels.active().viewport);
            let alignment_refresh_required = self.alignment_tracks.iter().any(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.projection(id).is_none_or(|projection| {
                        projection
                            .last_query_resolution
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
        self.alignment_tracks
            .iter()
            .filter(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.scene_engine.is_some()
            })
            .filter_map(|track| {
                let projection = track.projection(panel_id)?;
                if projection.query_is_current() {
                    return None;
                }
                let owner = projection.scene_owner?;
                let frame = projection.scene_frame.as_ref()?;
                if projection.desired_scene_camera != Some(frame.camera)
                    || !alignment_scene_event_token_is_current(
                        projection.desired_scene_request_token,
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
        let viewport = self.panels.active().viewport.interval();
        let cached_covers_viewport = cached
            .as_ref()
            .is_some_and(|slice| slice.interval.contains_interval(viewport));
        if matches!(lane_plan, AlignmentReferenceLanePlan::AllQueued) && cached_covers_viewport {
            let panel = self.panels.active_mut();
            panel.pending_queries.reference = false;
            panel.reference_settled_generation = Some(panel.generation_clock.current().get());
            panel.reference_query_error_generation = None;
            return;
        }
        let (revision, interval) = match lane_plan {
            AlignmentReferenceLanePlan::Query { revision, interval } => (revision, interval),
            AlignmentReferenceLanePlan::AllQueued => (revision, viewport),
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
            self.panels.active_mut().contig_name.clone(),
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
                self.data_status = EngineQueueError::Disconnected.to_string();
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
        let interval = self.panels.active().viewport.interval();
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
            interval.start(),
            interval.end(),
            track_ids,
            variant_settings,
            interaction_settings,
            copy_number_settings,
        ) {
            Ok(()) => self.panels.active_mut().pending_queries.tracks = false,
            Err(EngineQueueError::Full) => {
                "Track queue busy; retrying the latest locus…".clone_into(&mut self.data_status);
            }
            Err(EngineQueueError::Disconnected) => {
                self.panels.active_mut().pending_queries.tracks = false;
                self.data_status = EngineQueueError::Disconnected.to_string();
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
                    .is_some_and(|projection| projection.pending_query)
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
            self.panels.active_mut().pending_queries.alignment |= alignment_pending;
            self.dispatch_alignment_query();
            self.dispatch_reference_query();
            self.dispatch_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn poll_background_work(&mut self) {
        let now = Instant::now();
        self.dispatch_due_navigation_queries(now);
        self.dispatch_due_resolution_query_refresh(now);
        self.poll_reference_events();
        self.poll_alignment_events();
        self.poll_alignment_row_layout_tasks();
        self.poll_alignment_row_scene_tasks();
        self.poll_track_events();
        self.poll_alignment_retirement_tasks();
        self.poll_session_load_tasks();
        self.poll_session_save_tasks();
        self.poll_export_tasks();
        self.poll_workspace_list_import_tasks();
        self.retry_pending_queries();
        self.update_navigation_readiness();
        if self.reveal_due_alignment_checking_notices(now)
            || self.reveal_due_navigation_notices(now)
        {
            self.display_dirty = true;
        }
        if self.display_dirty {
            self.window.request_redraw();
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
                        self.data_status = error.to_string();
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
        for event in events {
            match event {
                ReferenceEngineEvent::Opened {
                    revision,
                    dictionary,
                    remote_io_stats,
                } => {
                    let reference_contig_count = dictionary.len();
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
                        panel.reference_request_revision = None;
                        panel.reference_request_interval = None;
                        panel.reference_installs_queued.clear();
                        panel.reference_settled_generation = None;
                        panel.reference_query_error_generation = None;
                        panel.pending_queries.reference = false;
                    }
                    if self.primary_alignment_dictionary().is_none() {
                        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
                        for id in ids {
                            let (requested, pixel_width) = self
                                .panels
                                .get(id)
                                .map(|panel| {
                                    (panel.requested_locus.clone(), panel.viewport.pixel_width())
                                })
                                .expect("panel id came from the panel collection");
                            match resolve_initial_locus(
                                &dictionary,
                                &self.contig_aliases,
                                requested.as_deref(),
                            )
                            .and_then(|(interval, name, length)| {
                                Ok((
                                    Viewport::new(interval, pixel_width)?,
                                    interval,
                                    name,
                                    length,
                                ))
                            }) {
                                Ok((viewport, interval, name, length)) => {
                                    let panel = self
                                        .panels
                                        .get_mut(id)
                                        .expect("panel id came from the panel collection");
                                    panel.generation_clock.advance();
                                    panel.viewport = viewport;
                                    panel.contig_name = name;
                                    panel.contig_length = length;
                                    panel.home_interval = interval;
                                    panel.navigation_history = NavigationHistory::new(interval);
                                    panel.vertical_scroll_points = 0.0;
                                    panel.vertical_content_origin_points = 0.0;
                                    panel.latent_vertical_scroll_fraction = None;
                                }
                                Err(error) if id == active_panel_id => {
                                    self.data_status = error.to_string();
                                }
                                Err(_) => {}
                            }
                        }
                        let default_annotation_count = self.assembly.default_annotation_count;
                        self.assembly = load_assembly(
                            &self.assembly_name,
                            dictionary.clone(),
                            self.cytoband_path.as_deref(),
                        );
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
                    }
                    self.refresh_active_panel_context();
                    self.request_all_panel_queries();
                    self.display_dirty = true;
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
                        panel.reference_request_revision = None;
                        panel.reference_request_interval = None;
                        panel.reference_query_error_generation = None;
                        self.display_dirty = true;
                        if is_active && !self.has_alignment_engine() {
                            "Reference sequence loaded".clone_into(&mut self.data_status);
                        }
                    }
                    if accepted {
                        let installed = self.install_reference_authority_for_panel(
                            request.panel_id(),
                            revision,
                            &slice,
                        );
                        let viewport_covered =
                            self.panels.get(request.panel_id()).is_some_and(|panel| {
                                slice.interval.contains_interval(panel.viewport.interval())
                            });
                        let settled = installed && viewport_covered;
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
                        self.reference_engine = None;
                        for panel in self.panels.iter_mut() {
                            panel.settle_reference_open_failure();
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
                                        projection.desired_scene_camera == Some(frame.camera)
                                            && alignment_scene_event_token_is_current(
                                                projection.desired_scene_request_token,
                                                projection.scene_frame_request_token,
                                            )
                                            && reference_failure_matches_requirement(
                                                revision,
                                                interval,
                                                frame.reference_requirement,
                                            )
                                    });
                                if matching_current_requirement {
                                    // A terminal FASTA dependency failure is not an alignment
                                    // source failure. Keep the immutable scene and its reads, but
                                    // let this transaction settle with the explicit reference
                                    // error already recorded on the panel.
                                    projection.finish_checking();
                                    projection.pending_query = false;
                                    projection.settled_generation =
                                        Some(projection.clock.current().get());
                                    if let Some(owner) = projection.scene_owner {
                                        failed_owners.push(owner);
                                    }
                                }
                            }
                            let alignment_pending = self.alignment_tracks.iter().any(|track| {
                                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
                                    && track
                                        .projection(panel_id)
                                        .is_some_and(|projection| projection.pending_query)
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
        self.window.request_redraw();
    }

    // Preserved only as a deletion oracle until the first desktop scene slice passes manual
    // acceptance. It is not compiled and cannot become a runtime fallback.
    #[cfg(any())]
    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_alignment_events(&mut self) {
        let mut events = Vec::new();
        let active_panel_id = self.panels.active().id;
        let primary_id = self.alignment_tracks.first().map(|track| track.config.id);
        for track in &mut self.alignment_tracks {
            let id = track.config.id;
            loop {
                match track.engine.try_receive() {
                    Ok(Some(event)) => events.push((id, event)),
                    Ok(None) => break,
                    Err(error) => {
                        track.lifecycle = AlignmentTrackLifecycle::Failed(error.to_string());
                        if Some(id) == primary_id {
                            self.data_status = error.to_string();
                        }
                        break;
                    }
                }
            }
        }
        let had_events = !events.is_empty();
        for (track_id, event) in events {
            let mut event_is_current = true;
            if let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == track_id)
            {
                match &event {
                    AlignmentEngineEvent::Opened {
                        dictionary,
                        index_overview,
                    } => {
                        track.lifecycle = AlignmentTrackLifecycle::Ready;
                        track.dictionary = Some(dictionary.clone());
                        track.index_overview.clone_from(index_overview);
                        for projection in track.projections.values_mut() {
                            projection.pending_query = true;
                        }
                    }
                    AlignmentEngineEvent::QueryCompleted {
                        request,
                        query,
                        layout_query,
                        layout_cache_hit,
                        cache_stats,
                        remote_io_stats,
                        working_set,
                        ..
                    } => {
                        let projection = track.projection_mut(request.panel_id());
                        event_is_current = projection.clock.is_current(request.generation());
                        if event_is_current {
                            projection.finish_checking();
                            projection.query_stats = Some(query.stats);
                            projection.current_query = Some(Arc::clone(query));
                            projection.layout_query = Some(Arc::clone(layout_query));
                            projection.layout_query_contract_revision =
                                self.navigation_contract_revision;
                            projection.layout_query_cache_hit = *layout_cache_hit;
                            projection.working_set = *working_set;
                            projection.unavailable = None;
                            projection.index_only_overview = false;
                            projection.settled_generation = Some(request.generation().get());
                            track.cache_stats = Some(*cache_stats);
                            track.remote_io_stats = *remote_io_stats;
                        }
                    }
                    AlignmentEngineEvent::WorkingSetUpdated {
                        request,
                        resident_query,
                        cache_stats,
                        remote_io_stats,
                        working_set,
                    } => {
                        let projection = track.projection_mut(request.panel_id());
                        event_is_current = projection.clock.is_current(request.generation());
                        if event_is_current {
                            projection.working_set = *working_set;
                            projection.resident_query = resident_query.clone();
                            track.cache_stats = Some(*cache_stats);
                            track.remote_io_stats = *remote_io_stats;
                        }
                    }
                    AlignmentEngineEvent::QueryPhaseChanged { request, phase } => {
                        let projection = track.projection_mut(request.panel_id());
                        event_is_current = projection.clock.is_current(request.generation());
                        if event_is_current {
                            projection.query_phase = *phase;
                        }
                    }
                    AlignmentEngineEvent::Error { request, message } => {
                        event_is_current = request.is_none_or(|request| {
                            track
                                .projection(request.panel_id())
                                .is_some_and(|projection| {
                                    projection.clock.is_current(request.generation())
                                })
                        });
                        if event_is_current {
                            if let Some(request) = request {
                                track.projection_mut(request.panel_id()).finish_checking();
                            }
                            track.lifecycle = AlignmentTrackLifecycle::Failed(message.clone());
                        }
                    }
                    AlignmentEngineEvent::CacheCleared { cache_stats }
                    | AlignmentEngineEvent::CacheBudgetChanged { cache_stats } => {
                        track.cache_stats = Some(*cache_stats);
                    }
                }
            }
            if !event_is_current
                && matches!(
                    &event,
                    AlignmentEngineEvent::QueryCompleted { .. }
                        | AlignmentEngineEvent::WorkingSetUpdated { .. }
                        | AlignmentEngineEvent::Error {
                            request: Some(_),
                            ..
                        }
                )
            {
                self.navigation_stale_result_rejections =
                    self.navigation_stale_result_rejections.saturating_add(1);
            }
            if matches!(event, AlignmentEngineEvent::Opened { .. }) {
                self.panels.active_mut().pending_queries.alignment = true;
            }
            if event_is_current && matches!(event, AlignmentEngineEvent::QueryCompleted { .. }) {
                // Results for inactive panels and secondary tracks still alter
                // the composite workspace even when they do not change the
                // active status line.
                self.display_dirty = true;
            }
            if Some(track_id) != primary_id {
                if event_is_current {
                    match &event {
                        AlignmentEngineEvent::QueryCompleted { request, query, .. } => {
                            self.resolve_pending_read_selection(
                                request.panel_id(),
                                track_id,
                                query,
                            );
                        }
                        AlignmentEngineEvent::Error { request, message }
                            if request
                                .is_none_or(|request| request.panel_id() == active_panel_id) =>
                        {
                            let name = self
                                .alignment_tracks
                                .iter()
                                .find(|track| track.config.id == track_id)
                                .map_or("Alignment track", |track| track.config.name.as_str());
                            self.data_status = format!("{name}: {message}");
                        }
                        _ => {}
                    }
                }
                continue;
            }
            match event {
                AlignmentEngineEvent::Opened { dictionary, .. } => {
                    if let Some(source) = self.primary_alignment_source().cloned() {
                        self.settings.remember_alignment(&source);
                        self.persist_settings();
                    }
                    let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
                    for id in ids {
                        let (requested, pixel_width) = self
                            .panels
                            .get(id)
                            .map(|panel| {
                                (panel.requested_locus.clone(), panel.viewport.pixel_width())
                            })
                            .expect("panel id came from the panel collection");
                        match resolve_initial_locus(
                            &dictionary,
                            &self.contig_aliases,
                            requested.as_deref(),
                        )
                        .and_then(|(interval, name, length)| {
                            Ok((
                                Viewport::new(interval, pixel_width)?,
                                interval,
                                name,
                                length,
                            ))
                        }) {
                            Ok((viewport, interval, name, length)) => {
                                let panel = self
                                    .panels
                                    .get_mut(id)
                                    .expect("panel id came from the panel collection");
                                panel.viewport = viewport;
                                panel.contig_name = name;
                                panel.contig_length = length;
                                panel.home_interval = interval;
                                panel.navigation_history = NavigationHistory::new(interval);
                                panel.vertical_scroll_points = 0.0;
                                panel.vertical_content_origin_points = 0.0;
                                panel.latent_vertical_scroll_fraction = None;
                            }
                            Err(error) if id == active_panel_id => {
                                self.data_status = error.to_string();
                            }
                            Err(_) => {}
                        }
                    }
                    let default_annotation_count = self.assembly.default_annotation_count;
                    self.assembly = load_assembly(
                        &self.assembly_name,
                        dictionary.clone(),
                        self.cytoband_path.as_deref(),
                    );
                    self.assembly.default_annotation_count = default_annotation_count;
                    self.assembly.reference_contig_count = self
                        .reference_dictionary
                        .as_ref()
                        .map_or(dictionary.len(), ReferenceDictionary::len);
                    self.assembly.identity.clone_from(&self.assembly_identity);
                    self.request_all_panel_queries();
                }
                AlignmentEngineEvent::QueryCompleted {
                    request,
                    query,
                    layout_query,
                    cache_hit,
                    cache_stats,
                    remote_io_stats,
                    ..
                } if event_is_current => {
                    self.update_landmark_resolution_from_header(&query.header);
                    let is_active = request.panel_id() == active_panel_id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id()) {
                        let has_pending_selection = panel
                            .pending_read_selection
                            .as_ref()
                            .is_some_and(|pending| pending.track_id == track_id);
                        panel.selected_record = None;
                        panel.selected_read_event = None;
                        if !has_pending_selection {
                            panel.selected_alignment_track = None;
                        }
                    }
                    self.resolve_pending_read_selection(request.panel_id(), track_id, &query);
                    if !is_active {
                        continue;
                    }
                    let cache_label = if cache_hit { "cache hit · " } else { "" };
                    let refusal = alignment::individual_read_refusal(
                        &query,
                        &layout_query,
                        self.primary_alignment_filters().read_stacking_mode,
                    );
                    self.data_status = if let Some(refusal) = refusal {
                        format!("{cache_label}{}", alignment::read_refusal_detail(refusal))
                    } else {
                        format!(
                            "{cache_label}{} complete alignments",
                            query.stats.records_emitted
                        )
                    };
                    if self
                        .panels
                        .active()
                        .navigation_presentation
                        .permits_staged_display()
                    {
                        self.rebuild_genomic_display();
                    }
                    self.display_dirty = true;
                }
                AlignmentEngineEvent::Error { request, message }
                    if event_is_current
                        && request.is_none_or(|request| request.panel_id() == active_panel_id) =>
                {
                    self.data_status = message;
                }
                AlignmentEngineEvent::CacheCleared { .. } => {
                    "Alignment cache cleared".clone_into(&mut self.data_status);
                }
                AlignmentEngineEvent::CacheBudgetChanged { .. } => {
                    "Cache budget updated".clone_into(&mut self.data_status);
                }
                AlignmentEngineEvent::QueryPhaseChanged { request, phase }
                    if event_is_current
                        && request.panel_id() == active_panel_id
                        && phase != AlignmentQueryPhase::Ready
                        && self.alignment_tracks.first().is_some_and(|track| {
                            track
                                .projection(request.panel_id())
                                .is_some_and(AlignmentPanelProjection::query_is_checking)
                        }) =>
                {
                    phase.status_text().clone_into(&mut self.data_status);
                }
                AlignmentEngineEvent::WorkingSetUpdated { .. } => {}
                AlignmentEngineEvent::QueryCompleted { .. }
                | AlignmentEngineEvent::QueryPhaseChanged { .. }
                | AlignmentEngineEvent::Error { .. } => {}
            }
        }
        if had_events {
            // Cache statistics arrive asynchronously from each alignment
            // actor. Recompute borrowing only after consuming the batch so a
            // settled trim cannot leave stale per-track allowances in place.
            self.rebalance_live_cache_coordinator();
            self.schedule_alignment_row_scene_preparations();
            self.window.request_redraw();
        }
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
                            self.data_status = error.to_string();
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
                    for projection in track.projections.values_mut() {
                        projection.pending_query = true;
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
                    ..
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
                    if !alignment_scene_frame_is_current(
                        projection.desired_scene_request_token,
                        request_token,
                        projection.desired_scene_camera,
                        frame.camera,
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
                    let current_target = projection.desired_scene_camera == Some(frame.camera);
                    if current_target {
                        projection.pending_query = false;
                        projection.unavailable = None;
                        projection.index_only_overview = false;
                        if matches!(
                            frame.reference_requirement,
                            AlignmentReferenceRequirement::Pending { .. }
                        ) {
                            // Source/layout work is complete, but Base pages cannot become the
                            // current generation until the exact versioned FASTA horizon is
                            // installed and a reference-backed frame is published.
                            reference_panels_to_request.insert(panel_id);
                        } else if frame.visible_complete() {
                            projection.finish_checking();
                            projection.settled_generation = Some(projection.clock.current().get());
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
                    rebuild_display |= published_scene.is_some()
                        && cause != AlignmentSceneFrameCause::VerticalResidency;
                    prepare_frame = true;
                }
                AlignmentSceneEngineEvent::IndividualReadsRefused {
                    owner,
                    request_token,
                    target,
                    complete_read_limit,
                    eligible_records,
                    frame,
                    ..
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
                        projection.desired_scene_request_token,
                        request_token,
                        projection.desired_scene_camera,
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
                    if projection.desired_scene_camera == Some(frame.camera) {
                        projection.finish_checking();
                        projection.pending_query = false;
                        projection.settled_generation = Some(projection.clock.current().get());
                    }
                    if Some(track_id) == primary_id && panel_id == active_panel_id {
                        self.data_status = format!(
                            "{eligible_records} eligible alignments exceed the Complete read limit of {complete_read_limit}"
                        );
                    }
                    rebuild_display = true;
                    prepare_frame = true;
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
                            projection.desired_scene_request_token,
                            request_token,
                        ) {
                            self.navigation_stale_result_rejections =
                                self.navigation_stale_result_rejections.saturating_add(1);
                            continue;
                        }
                        projection.settle_unavailable(message.to_string());
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
                            projection.pending_query = false;
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
                            projection.settle_unavailable(message.to_string());
                        }
                    }
                    if Some(track_id) == primary_id
                        && panel_id.is_none_or(|panel_id| panel_id == active_panel_id)
                    {
                        self.data_status = message.to_string();
                    }
                    rebuild_display = true;
                }
                AlignmentSceneEngineEvent::LaneInserted { .. }
                | AlignmentSceneEngineEvent::LaneRemoved { .. }
                | AlignmentSceneEngineEvent::PresentationPrewarmed { .. }
                | AlignmentSceneEngineEvent::SourceControlQueued
                | AlignmentSceneEngineEvent::BudgetReduced { .. } => {}
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
                    && track
                        .projection(panel_id)
                        .is_some_and(|projection| projection.pending_query)
            });
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel.pending_queries.alignment = pending;
            }
        }

        if let Some(dictionary) = opened_primary_dictionary {
            if let Some(source) = self.primary_alignment_source().cloned() {
                self.settings.remember_alignment(&source);
                self.persist_settings();
            }
            let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
            for id in ids {
                let (requested, pixel_width) = self
                    .panels
                    .get(id)
                    .map(|panel| (panel.requested_locus.clone(), panel.viewport.pixel_width()))
                    .expect("panel id came from the panel collection");
                match resolve_initial_locus(&dictionary, &self.contig_aliases, requested.as_deref())
                    .and_then(|(interval, name, length)| {
                        Ok((
                            Viewport::new(interval, pixel_width)?,
                            interval,
                            name,
                            length,
                        ))
                    }) {
                    Ok((viewport, interval, name, length)) => {
                        let panel = self
                            .panels
                            .get_mut(id)
                            .expect("panel id came from the panel collection");
                        panel.viewport = viewport;
                        panel.contig_name = name;
                        panel.contig_length = length;
                        panel.home_interval = interval;
                        panel.navigation_history = NavigationHistory::new(interval);
                        panel.vertical_scroll_points = 0.0;
                        panel.vertical_content_origin_points = 0.0;
                        panel.latent_vertical_scroll_fraction = None;
                    }
                    Err(error) if id == active_panel_id => self.data_status = error.to_string(),
                    Err(_) => {}
                }
            }
            let default_annotation_count = self.assembly.default_annotation_count;
            self.assembly = load_assembly(
                &self.assembly_name,
                dictionary.clone(),
                self.cytoband_path.as_deref(),
            );
            self.assembly.default_annotation_count = default_annotation_count;
            self.assembly.reference_contig_count = self
                .reference_dictionary
                .as_ref()
                .map_or(dictionary.len(), ReferenceDictionary::len);
            self.assembly.identity.clone_from(&self.assembly_identity);
        }
        if source_opened {
            // Rebuild first so dispatch can derive the read-pane clip from committed layout.
            self.rebuild_all_panel_displays();
            self.request_all_panel_queries();
            rebuild_display = false;
            prepare_frame = true;
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
    pub(crate) fn poll_track_events(&mut self) {
        let mut events = Vec::new();
        if let Some(engine) = &self.track_engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        self.data_status = error.to_string();
                        break;
                    }
                }
            }
        }
        let had_events = !events.is_empty();
        for event in events {
            let stale_query_result = match &event {
                TrackEngineEvent::QueryCompleted { request, .. }
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
                    self.display_dirty = true;
                }
                TrackEngineEvent::TdfChannelsDiscovered {
                    id,
                    track_names,
                    selected_index,
                } => {
                    if let Err(error) = self.expand_tdf_channels(id, &track_names, selected_index) {
                        self.data_status = error.to_string();
                    }
                }
                TrackEngineEvent::QueryCompleted { request, tracks } => {
                    let is_active = request.panel_id() == self.panels.active().id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                    {
                        panel.track_snapshots = tracks;
                        panel.track_snapshot_interval = Some(panel.viewport.interval());
                        panel.track_settled_generation = Some(request.generation().get());
                        panel.track_query_error_generation = None;
                        panel.selected_feature = None;
                        panel.selected_variant = None;
                        panel.selected_gwas = None;
                        panel.selected_quantitative = None;
                        panel.selected_copy_number = None;
                        panel.selected_mutation = None;
                        panel.selected_interaction = None;
                        self.display_dirty = true;
                        if is_active
                            && self
                                .panels
                                .active()
                                .navigation_presentation
                                .permits_staged_display()
                        {
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
                    self.display_dirty = true;
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
                    track: _,
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
                                panel.track_query_error_generation =
                                    Some(request.generation().get());
                            }
                            self.display_dirty = true;
                            if is_active {
                                self.data_status = message;
                            }
                        }
                    } else {
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
