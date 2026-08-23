use super::super::*;

const MAXIMUM_AUTOMATION_SUPPLEMENTARY_SEGMENTS: usize = 64;

fn automation_locus(contig: &str, interval: GenomicInterval) -> String {
    format!(
        "{}:{}-{}",
        contig,
        interval.start().saturating_add(1),
        interval.end()
    )
}

fn bounded_automation_text(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    format!("{}…", &value[..boundary])
}

fn automation_supplementary_segment(
    record: &AlignmentRecord,
    reference_name: &str,
) -> SupplementarySegment {
    let edit_distance = record
        .auxiliary_fields
        .iter()
        .find(|field| field.tag == *b"NM")
        .and_then(|field| match field.value {
            AuxValue::Signed(value) => u32::try_from(value).ok(),
            AuxValue::Unsigned(value) => u32::try_from(value).ok(),
            _ => None,
        })
        .unwrap_or_default();
    SupplementarySegment {
        reference_name: reference_name.to_owned(),
        start: record.start,
        end: record.end,
        reverse: record.flags.is_reverse(),
        cigar: record.cigar.clone(),
        mapping_quality: record.mapping_quality,
        edit_distance,
    }
}

fn automation_query_supplementary_chain(
    query: &AlignmentQuery,
    record: &AlignmentRecord,
    current_contig: &str,
) -> Result<Option<SelectedSupplementaryChain>> {
    let mut chain = selected_supplementary_chain(record, current_contig).unwrap_or_else(|| {
        SelectedSupplementaryChain {
            read_name: record.read_name.clone(),
            segments: vec![automation_supplementary_segment(record, current_contig)],
            active_segment: 0,
            malformed_entries: 0,
            truncated: false,
            invalid_tag_type: false,
        }
    });
    for index in 0..query.batch.len() {
        if chain.segments.len() >= MAXIMUM_AUTOMATION_SUPPLEMENTARY_SEGMENTS {
            chain.truncated = true;
            break;
        }
        let candidate_name = query.batch.read_name(index).unwrap_or_default();
        if candidate_name != record.read_name
            || (!record.flags.is_supplementary() && !query.batch.flags[index].is_supplementary())
        {
            continue;
        }
        let candidate = query.materialize_record(index)?;
        let segment = automation_supplementary_segment(&candidate, current_contig);
        if !chain.segments.iter().any(|existing| {
            existing.reference_name == segment.reference_name
                && existing.start == segment.start
                && existing.reverse == segment.reverse
                && existing.cigar == segment.cigar
        }) {
            chain.segments.push(segment);
        }
    }
    Ok((chain.segments.len() > 1).then_some(chain))
}

pub(crate) fn automation_read_evidence_field_is_public(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    !matches!(
        normalized.as_str(),
        "read" | "sequence" | "sequence copy orientation" | "template identity"
    ) && !normalized.contains("read group")
}

pub(crate) fn observed_retained_selection(
    retained: &AutomationSelectionSnapshot,
    viewport: GenomicInterval,
) -> AutomationSelectionSnapshot {
    let mut snapshot = retained.clone();
    snapshot.visible_in_active_view = snapshot
        .evidence_start
        .zip(snapshot.evidence_end)
        .and_then(|(start, end)| GenomicInterval::new(viewport.contig(), start, end).ok())
        .is_none_or(|evidence| viewport.contains_interval(evidence));
    snapshot
}

pub(crate) fn automation_viewport_after_operation(
    current: Viewport,
    contig_length: u64,
    operation: AutomationViewportOperation,
) -> Result<Viewport> {
    Ok(match operation {
        AutomationViewportOperation::PanBases { delta } => {
            current.pan_pixels(delta as f64 / current.bases_per_pixel(), contig_length)?
        }
        AutomationViewportOperation::PanFraction { fraction } => {
            current.pan_pixels(current.pixel_width() * fraction, contig_length)?
        }
        AutomationViewportOperation::Zoom {
            factor,
            anchor_position,
        } => {
            let anchor_x = anchor_position.map_or(current.pixel_width() / 2.0, |position| {
                current.pixel_at_genomic(position)
            });
            current.zoom_at(anchor_x, factor.recip(), contig_length)?
        }
        AutomationViewportOperation::FitInterval { start, end } => {
            let bounded_length = contig_length.max(1);
            let start = start.min(bounded_length.saturating_sub(1));
            let end = end.min(bounded_length).max(start.saturating_add(1));
            let interval =
                GenomicInterval::bounded(current.interval().contig(), start, end, contig_length)?;
            Viewport::new(interval, current.pixel_width())?
        }
    })
}

impl Viewer {
    pub(crate) fn automation_is_settled(&self) -> bool {
        !self.display_dirty && self.automation_data_is_settled()
    }

    /// Whether the observable workspace data has settled. Receiving a local-control
    /// request dirties the status bar before its first command runs, so observation
    /// snapshots must not treat that control-only repaint as unsettled genomic data.
    fn automation_data_is_settled(&self) -> bool {
        if !self.export_tasks.is_empty()
            || !self.session_save_tasks.is_empty()
            || !self.session_load_tasks.is_empty()
            || !self.workspace_list_import_tasks.is_empty()
        {
            return false;
        }
        if self.reference_engine.is_some() && self.reference_dictionary.is_none() {
            return false;
        }
        for panel in self.panels.iter() {
            if panel.pending_queries.alignment
                || panel.pending_queries.reference
                || panel.pending_queries.tracks
            {
                return false;
            }
            if self.track_engine.is_some()
                && (panel.track_snapshot_interval != Some(panel.viewport.interval())
                    || panel.track_settled_generation
                        != Some(panel.generation_clock.current().get()))
            {
                return false;
            }
            if self.reference_engine.is_some()
                && panel.viewport.detail_level() == DetailLevel::Bases
                && (panel.reference_slice.as_ref().is_none_or(|slice| {
                    !slice.interval.contains_interval(panel.viewport.interval())
                }) || panel.reference_settled_generation
                    != Some(panel.generation_clock.current().get()))
            {
                return false;
            }
            for track in self
                .alignment_tracks
                .iter()
                .filter(|track| track.config.visible)
            {
                match track.lifecycle {
                    AlignmentTrackLifecycle::Opening => return false,
                    AlignmentTrackLifecycle::Ready => {
                        let Some(projection) = track.projection(panel.id) else {
                            return false;
                        };
                        if projection.pending_query
                            || projection.settled_generation
                                != Some(projection.clock.current().get())
                            || (projection.current_query.is_none()
                                && projection.unavailable.is_none())
                        {
                            return false;
                        }
                    }
                    AlignmentTrackLifecycle::Failed(_) => {}
                }
            }
        }
        true
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn advance_automation(&mut self, now: Instant) {
        self.maybe_start_control_request();
        let Some(runner) = self.automation.as_mut() else {
            return;
        };
        if runner.finished {
            return;
        }
        if runner
            .report_worker
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            let worker = runner.report_worker.take().unwrap();
            let exit = runner.exit_after_report;
            match worker.join() {
                Ok(Ok(())) => {
                    if let Some(path) = &runner.report_path {
                        self.data_status =
                            format!("Automation report written to {}", path.display());
                    }
                }
                Ok(Err(error)) => {
                    self.data_status = format!("Could not write automation report: {error}");
                }
                Err(_) => {
                    "Automation-report worker failed".clone_into(&mut self.data_status);
                }
            }
            runner.finished = true;
            self.automation_exit_requested = exit;
            self.display_dirty = true;
            return;
        }
        if runner.report_worker.is_some() {
            return;
        }
        let waiting = runner.waiting.clone();
        match waiting {
            Some(AutomationWait::Settled {
                command_index,
                deadline,
            }) => {
                if self.automation_is_settled() {
                    if let Some(runner) = self.automation.as_mut() {
                        runner.record_first_settled(now);
                    }
                    self.finish_automation_command(
                        command_index,
                        Ok((
                            "Workspace reached a deterministic settled state".to_owned(),
                            serde_json::json!({"settled": true}),
                        )),
                    );
                } else if now >= deadline {
                    self.finish_automation_command(
                        command_index,
                        Err((
                            AutomationErrorCode::Timeout,
                            "Workspace did not settle before the declared timeout".to_owned(),
                        )),
                    );
                }
                return;
            }
            Some(AutomationWait::File { .. }) => return,
            Some(AutomationWait::Reference {
                command_index,
                deadline,
            }) => {
                if let Some(error) = self.reference_open_error.clone() {
                    self.finish_automation_command(
                        command_index,
                        Err((AutomationErrorCode::ResourceNotFound, error)),
                    );
                } else if self.reference_dictionary.is_some() {
                    self.finish_automation_command(
                        command_index,
                        Ok((
                            "Opened the reference genome".to_owned(),
                            serde_json::json!({"reference_ready": true}),
                        )),
                    );
                } else if now >= deadline {
                    self.finish_automation_command(
                        command_index,
                        Err((
                            AutomationErrorCode::Timeout,
                            "Reference genome did not open before the automation timeout"
                                .to_owned(),
                        )),
                    );
                }
                return;
            }
            Some(AutomationWait::AlignmentTrack {
                command_index,
                track_id,
                deadline,
            }) => {
                let lifecycle = self
                    .alignment_tracks
                    .iter()
                    .find(|track| track.config.id == track_id)
                    .map(|track| track.lifecycle.clone());
                match lifecycle {
                    Some(AlignmentTrackLifecycle::Ready) => self.finish_automation_command(
                        command_index,
                        Ok((
                            format!("Opened alignment track {}", track_id.get()),
                            serde_json::json!({"track_id": track_id.get()}),
                        )),
                    ),
                    Some(AlignmentTrackLifecycle::Failed(error)) => self.finish_automation_command(
                        command_index,
                        Err((AutomationErrorCode::ResourceNotFound, error)),
                    ),
                    None => self.finish_automation_command(
                        command_index,
                        Err((
                            AutomationErrorCode::ResourceNotFound,
                            format!(
                                "Alignment track {} disappeared while opening",
                                track_id.get()
                            ),
                        )),
                    ),
                    Some(AlignmentTrackLifecycle::Opening) if now >= deadline => self
                        .finish_automation_command(
                            command_index,
                            Err((
                                AutomationErrorCode::Timeout,
                                format!(
                                    "Alignment track {} did not open before the automation timeout",
                                    track_id.get()
                                ),
                            )),
                        ),
                    Some(AlignmentTrackLifecycle::Opening) => {}
                }
                return;
            }
            Some(AutomationWait::AuxiliaryTrack {
                command_index,
                track_id,
                deadline,
            }) => {
                match self.automation_auxiliary_open_state.get(&track_id).cloned() {
                    Some(AutomationAuxiliaryOpenState::Ready) => self.finish_automation_command(
                        command_index,
                        Ok((
                            format!("Opened auxiliary track {}", track_id.get()),
                            serde_json::json!({"track_id": track_id.get()}),
                        )),
                    ),
                    Some(AutomationAuxiliaryOpenState::Failed(error)) => self
                        .finish_automation_command(
                            command_index,
                            Err((AutomationErrorCode::ResourceNotFound, error)),
                        ),
                    None => self.finish_automation_command(
                        command_index,
                        Err((
                            AutomationErrorCode::ResourceNotFound,
                            format!(
                                "Auxiliary track {} disappeared while opening",
                                track_id.get()
                            ),
                        )),
                    ),
                    Some(AutomationAuxiliaryOpenState::Pending) if now >= deadline => self
                        .finish_automation_command(
                            command_index,
                            Err((
                                AutomationErrorCode::Timeout,
                                format!(
                                    "Auxiliary track {} did not open before the automation timeout",
                                    track_id.get()
                                ),
                            )),
                        ),
                    Some(AutomationAuxiliaryOpenState::Pending) => {}
                }
                return;
            }
            Some(AutomationWait::ExactBaseSelection {
                command_index,
                panel_id,
                track_id,
                position,
                previous_selection,
                deadline,
            }) => {
                if self.automation_is_settled() {
                    let selection = AutomationSelection::Position {
                        panel_id,
                        track_id,
                        position,
                        granularity: AutomationSelectionGranularity::ExactBase,
                    };
                    let outcome = self
                        .apply_automation_selection(&selection)
                        .map(|mut data| {
                            if let Some((selection_id, evidence_revision)) = previous_selection
                                && let Some(object) = data.as_object_mut()
                            {
                                object.insert(
                                    "previous_selection_id".to_owned(),
                                    serde_json::json!(selection_id),
                                );
                                object.insert(
                                    "previous_evidence_revision".to_owned(),
                                    serde_json::json!(evidence_revision),
                                );
                            }
                            ("Selected exact-base evidence".to_owned(), data)
                        })
                        .map_err(|error| {
                            (
                                AutomationErrorCode::EffectiveStateUnavailable,
                                error.to_string(),
                            )
                        });
                    self.finish_automation_command(command_index, outcome);
                } else if now >= deadline {
                    self.finish_automation_command(
                        command_index,
                        Err((
                            AutomationErrorCode::Timeout,
                            "exact-base evidence query did not settle before the timeout"
                                .to_owned(),
                        )),
                    );
                }
                return;
            }
            None => {}
        }

        let next = self
            .automation
            .as_mut()
            .and_then(|runner| runner.commands.pop_front());
        let Some((command_index, command)) = next else {
            self.start_automation_report();
            return;
        };
        if let Some(runner) = self.automation.as_mut() {
            runner.begin_command(command_index, command.name(), now);
        }
        let exit_command = matches!(command, AutomationCommand::Exit);
        match self.execute_automation_command(command_index, command, now) {
            AutomationExecution::Complete { message, data } => {
                if exit_command && let Some(runner) = self.automation.as_mut() {
                    runner.exit_after_report = true;
                }
                self.finish_automation_command(command_index, Ok((message, data)));
            }
            AutomationExecution::Wait(wait) => {
                if let Some(runner) = self.automation.as_mut() {
                    runner.waiting = Some(wait);
                }
            }
            AutomationExecution::Failed { code, message } => {
                self.finish_automation_command(command_index, Err((code, message)));
            }
        }
    }

    pub(crate) fn maybe_start_control_request(&mut self) {
        if self
            .automation
            .as_ref()
            .is_some_and(|runner| !runner.finished)
        {
            return;
        }
        let request = self
            .control_service
            .as_ref()
            .and_then(ControlService::try_request);
        let Some(request) = request else {
            return;
        };
        let command_count = request.batch.commands.len();
        self.automation = Some(AutomationRunner::for_control(request));
        self.data_status =
            format!("Running authenticated local-control batch ({command_count} commands)…");
        self.display_dirty = true;
    }

    pub(crate) fn finish_automation_command(
        &mut self,
        command_index: usize,
        outcome: std::result::Result<(String, serde_json::Value), (AutomationErrorCode, String)>,
    ) {
        let outcome = outcome.map(|(message, mut data)| {
            if let Some(object) = data.as_object_mut() {
                object.insert(
                    "application_instance_id".to_owned(),
                    serde_json::json!(self.application_instance_id),
                );
                object.insert(
                    "workspace_revision".to_owned(),
                    serde_json::json!(self.workspace_revision),
                );
            }
            (message, data)
        });
        let Some(runner) = self.automation.as_mut() else {
            return;
        };
        if !runner.finish_command(command_index, outcome) {
            let waiting_index = runner.waiting.as_ref().map(|waiting| match waiting {
                AutomationWait::Settled { command_index, .. }
                | AutomationWait::File { command_index }
                | AutomationWait::Reference { command_index, .. }
                | AutomationWait::AlignmentTrack { command_index, .. }
                | AutomationWait::AuxiliaryTrack { command_index, .. }
                | AutomationWait::ExactBaseSelection { command_index, .. } => *command_index,
            });
            tracing::warn!(
                command_index,
                ?waiting_index,
                "ignored a mismatched automation completion"
            );
            return;
        }
        self.display_dirty = true;
    }

    pub(crate) fn start_automation_report(&mut self) {
        let Some(runner) = self.automation.as_mut() else {
            return;
        };
        if runner.report_worker.is_some() {
            return;
        }
        let report = runner.report(Instant::now());
        if let Some(reply) = runner.control_reply.take() {
            let succeeded = report.succeeded;
            if reply.send(report).is_err() {
                "Local-control client disconnected before receiving the result"
                    .clone_into(&mut self.data_status);
            } else {
                self.data_status = if succeeded {
                    "Authenticated local-control batch completed".to_owned()
                } else {
                    "Authenticated local-control batch failed; a structured result was returned"
                        .to_owned()
                };
            }
            runner.finished = true;
            self.automation_exit_requested = runner.exit_after_report;
            self.display_dirty = true;
            return;
        }
        let json = match report.to_json() {
            Ok(json) => json,
            Err(error) => {
                self.data_status = format!("Could not serialize automation report: {error}");
                runner.finished = true;
                self.automation_exit_requested = runner.exit_after_report;
                return;
            }
        };
        let Some(path) = runner.report_path.clone() else {
            "Automation has no report destination".clone_into(&mut self.data_status);
            runner.finished = true;
            self.automation_exit_requested = runner.exit_after_report;
            return;
        };
        runner.report_worker = match std::thread::Builder::new()
            .name("rgv-automation-report".to_owned())
            .spawn(move || atomic_write_text(&path, &json))
        {
            Ok(worker) => Some(worker),
            Err(error) => {
                self.data_status = format!("Could not start automation-report writer: {error}");
                None
            }
        };
        if runner.report_worker.is_none() {
            runner.finished = true;
            self.automation_exit_requested = runner.exit_after_report;
        }
    }

    pub(crate) fn finish_automation_tasks(&mut self) {
        let Some(runner) = self.automation.as_mut() else {
            return;
        };
        if let Some(worker) = runner.report_worker.take() {
            match worker.join() {
                Ok(Ok(())) => {
                    runner.finished = true;
                    return;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "automation report failed during shutdown; retrying");
                }
                Err(_) => tracing::warn!("automation-report worker panicked during shutdown"),
            }
        }
        if runner.finished {
            return;
        }
        let active = runner.waiting.as_ref().map(|waiting| match waiting {
            AutomationWait::Settled { command_index, .. }
            | AutomationWait::File { command_index }
            | AutomationWait::Reference { command_index, .. }
            | AutomationWait::AlignmentTrack { command_index, .. }
            | AutomationWait::AuxiliaryTrack { command_index, .. }
            | AutomationWait::ExactBaseSelection { command_index, .. } => *command_index,
        });
        if let Some(command_index) = active {
            let _ = runner.finish_command(
                command_index,
                Err((
                    AutomationErrorCode::InvalidState,
                    "Application closed before this command completed".to_owned(),
                )),
            );
        } else if let Some((command_index, _)) = runner.commands.pop_front() {
            let _ = runner.finish_command(
                command_index,
                Err((
                    AutomationErrorCode::InvalidState,
                    "Application closed before this command started".to_owned(),
                )),
            );
        }
        let report = runner.report(Instant::now());
        if let Some(reply) = runner.control_reply.take() {
            if reply.send(report).is_err() {
                tracing::warn!("local-control result could not be preserved during shutdown");
            }
            runner.finished = true;
            return;
        }
        let Some(path) = runner.report_path.as_deref() else {
            tracing::warn!("automation result had no completion destination during shutdown");
            return;
        };
        match report
            .to_json()
            .map_err(|error| error.to_string())
            .and_then(|json| atomic_write_text(path, &json))
        {
            Ok(()) => runner.finished = true,
            Err(error) => tracing::warn!(
                %error,
                path = %path.display(),
                "automation report could not be preserved during shutdown"
            ),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn execute_automation_command(
        &mut self,
        command_index: usize,
        command: AutomationCommand,
        now: Instant,
    ) -> AutomationExecution {
        match command {
            AutomationCommand::DescribeWorkspace => {
                let snapshot = self.automation_workspace_snapshot();
                match serde_json::to_value(snapshot) {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Described the live workspace".to_owned(),
                        data,
                    },
                    Err(error) => automation_failure(AutomationErrorCode::Internal, error),
                }
            }
            AutomationCommand::DescribeSelection => {
                let selection = self.automation_selection_snapshot();
                let message = if selection.is_some() {
                    "Described the current semantic selection"
                } else {
                    "The active panel has no semantic selection"
                };
                match serde_json::to_value(selection) {
                    Ok(data) => AutomationExecution::Complete {
                        message: message.to_owned(),
                        data,
                    },
                    Err(error) => automation_failure(AutomationErrorCode::Internal, error),
                }
            }
            AutomationCommand::ClearSelection => {
                let previous = self.automation_selection_snapshot();
                self.panels.active_mut().clear_selection();
                self.bump_workspace_revision();
                self.rebuild_genomic_display();
                AutomationExecution::Complete {
                    message: "Cleared the active panel selection".to_owned(),
                    data: serde_json::json!({
                        "previous_selection_id": previous.map(|selection| selection.selection_id),
                        "workspace_revision": self.workspace_revision,
                    }),
                }
            }
            AutomationCommand::RefreshSelection => {
                let Some(previous) = self.automation_selection_snapshot() else {
                    return automation_failure(
                        AutomationErrorCode::NoSelection,
                        "the active panel has no selection to refresh",
                    );
                };
                let granularity = previous.granularity;
                if granularity == Some(AutomationSelectionGranularity::ExactBase)
                    && let (Some(track_id), Some(position)) =
                        (previous.track_id, previous.anchor_position)
                    && !self.automation_exact_base_available(previous.panel_id, track_id, position)
                {
                    if let Err(error) = self.apply_automation_viewport_operation(
                        previous.panel_id,
                        AutomationViewportOperation::FitInterval {
                            start: position,
                            end: position.saturating_add(1),
                        },
                    ) {
                        return automation_failure(AutomationErrorCode::InvalidCommand, error);
                    }
                    return AutomationExecution::Wait(AutomationWait::ExactBaseSelection {
                        command_index,
                        panel_id: previous.panel_id,
                        track_id,
                        position,
                        previous_selection: Some((
                            previous.selection_id,
                            previous.evidence_revision,
                        )),
                        deadline: now + Duration::from_secs(30),
                    });
                }
                self.panels.active_mut().retained_evidence = None;
                self.bump_workspace_revision();
                self.retain_active_selection(granularity);
                let Some(refreshed) = self.automation_selection_snapshot() else {
                    return automation_failure(
                        AutomationErrorCode::EffectiveStateUnavailable,
                        "the selected evidence is not available in the current query",
                    );
                };
                AutomationExecution::Complete {
                    message: "Refreshed selected evidence explicitly".to_owned(),
                    data: serde_json::json!({
                        "previous_selection_id": previous.selection_id,
                        "previous_evidence_revision": previous.evidence_revision,
                        "selection": refreshed,
                        "workspace_revision": self.workspace_revision,
                    }),
                }
            }
            AutomationCommand::DescribeReviewQueue => AutomationExecution::Complete {
                message: "Described the bounded review queue".to_owned(),
                data: self.automation_review_queue_snapshot(),
            },
            AutomationCommand::DescribeNavigationHistory { panel_id } => {
                match self.automation_navigation_history_snapshot(panel_id) {
                    Ok(data) => AutomationExecution::Complete {
                        message: format!("Described panel {} navigation history", panel_id.get()),
                        data,
                    },
                    Err(error) => automation_failure(AutomationErrorCode::ResourceNotFound, error),
                }
            }
            AutomationCommand::NewWorkspace => {
                let interval = self.panels.active().viewport.interval();
                let mut session = SessionDocument::new(SessionLocus {
                    contig: self.panels.active().contig_name.clone(),
                    start: interval.start(),
                    end: interval.end(),
                });
                session.reference = self.reference_source.clone();
                session.genome_name = Some(self.assembly_name.clone());
                session.cytobands = self.cytoband_path.as_deref().map(track_source);
                match self.apply_session(&session) {
                    Ok(()) => {
                        self.bump_workspace_revision();
                        AutomationExecution::Complete {
                            message: "Started a new workspace while retaining the active genome"
                                .to_owned(),
                            data: serde_json::json!({"panel_id": self.panels.active_mut().id.get()}),
                        }
                    }
                    Err(error) => automation_failure(AutomationErrorCode::InvalidState, error),
                }
            }
            AutomationCommand::OpenWorkspace {
                path,
                confirm_replace,
                unsaved_changes,
                timeout_millis: _,
            } => {
                if !confirm_replace {
                    return automation_failure(
                        AutomationErrorCode::WorkspaceReplacementNotConfirmed,
                        "workspace replacement was not confirmed",
                    );
                }
                let path = PathBuf::from(path);
                let dirty = self.workspace_revision != self.last_saved_workspace_revision;
                if dirty {
                    match unsaved_changes {
                        AutomationUnsavedChangesPolicy::Reject => {
                            return automation_failure(
                                AutomationErrorCode::UnsavedChanges,
                                "the visible workspace has unsaved changes",
                            );
                        }
                        AutomationUnsavedChangesPolicy::SaveThenOpen => {
                            let Some(current_path) = self.current_session_path.clone() else {
                                return automation_failure(
                                    AutomationErrorCode::UnsavedChanges,
                                    "save_then_open requires an existing project destination",
                                );
                            };
                            if let Err(error) = self.session_document().write(&current_path) {
                                return automation_failure(AutomationErrorCode::Io, error);
                            }
                            self.last_saved_workspace_revision = self.workspace_revision;
                        }
                        AutomationUnsavedChangesPolicy::Discard => {}
                    }
                }
                let session = match SessionDocument::read(&path) {
                    Ok(session) => session,
                    Err(error) => return automation_failure(AutomationErrorCode::Io, error),
                };
                if let Err(error) = self.apply_session(&session) {
                    return automation_failure(AutomationErrorCode::InvalidState, error);
                }
                self.current_session_path = Some(path);
                self.bump_workspace_revision();
                self.last_saved_workspace_revision = self.workspace_revision;
                AutomationExecution::Complete {
                    message: "Opened the requested workspace".to_owned(),
                    data: serde_json::json!({
                        "application_instance_id": self.application_instance_id,
                        "workspace_revision": self.workspace_revision,
                        "settled": self.automation_is_settled(),
                        "panels": self.automation_workspace_snapshot().panels,
                    }),
                }
            }
            AutomationCommand::SetGenome { source, name } => {
                let TrackSource::Local { path } = source else {
                    return automation_failure(
                        AutomationErrorCode::UnsupportedCommand,
                        "Remote standalone FASTA is not yet supported",
                    );
                };
                if !Path::new(&path).is_file() {
                    return automation_failure(
                        AutomationErrorCode::ResourceNotFound,
                        format!("reference genome {path} does not exist or is not a file"),
                    );
                }
                if let Some(name) = name {
                    self.assembly_name = name;
                }
                self.open_reference_source(Path::new(&path));
                if self.reference_engine.is_none() {
                    return automation_failure(
                        AutomationErrorCode::InvalidState,
                        &self.data_status,
                    );
                }
                self.bump_workspace_revision();
                AutomationExecution::Wait(AutomationWait::Reference {
                    command_index,
                    deadline: now + AUTOMATION_SOURCE_OPEN_TIMEOUT,
                })
            }
            AutomationCommand::LoadTrack { source, kind, name } => {
                let path = source_path(&source);
                if matches!(&source, TrackSource::Local { .. }) && !path.is_file() {
                    return automation_failure(
                        AutomationErrorCode::ResourceNotFound,
                        format!(
                            "track source {} does not exist or is not a file",
                            path.display()
                        ),
                    );
                }
                let effective_kind = inferred_automation_track_kind(&source, kind);
                let before = self
                    .alignment_tracks
                    .iter()
                    .map(|track| track.config.id)
                    .chain(self.tracks.tracks().iter().map(|track| track.id))
                    .collect::<BTreeSet<_>>();
                match effective_kind {
                    TrackKind::Reference => {
                        if matches!(source, TrackSource::Remote { .. }) {
                            return automation_failure(
                                AutomationErrorCode::UnsupportedCommand,
                                "Remote standalone FASTA is not yet supported",
                            );
                        }
                        self.open_reference_source(&path);
                        if self.reference_engine.is_none() {
                            return automation_failure(
                                AutomationErrorCode::InvalidState,
                                &self.data_status,
                            );
                        }
                        AutomationExecution::Wait(AutomationWait::Reference {
                            command_index,
                            deadline: now + AUTOMATION_SOURCE_OPEN_TIMEOUT,
                        })
                    }
                    TrackKind::Alignment => {
                        self.open_alignment_source(&path);
                        let track_id = self
                            .alignment_tracks
                            .iter()
                            .find(|track| !before.contains(&track.config.id))
                            .map(|track| track.config.id);
                        let Some(track_id) = track_id else {
                            return automation_failure(
                                AutomationErrorCode::InvalidState,
                                &self.data_status,
                            );
                        };
                        if let Some(name) = name
                            && let Some(track) = self
                                .alignment_tracks
                                .iter_mut()
                                .find(|track| track.config.id == track_id)
                        {
                            track.config.name = name;
                        }
                        self.bump_workspace_revision();
                        AutomationExecution::Wait(AutomationWait::AlignmentTrack {
                            command_index,
                            track_id,
                            deadline: now + AUTOMATION_SOURCE_OPEN_TIMEOUT,
                        })
                    }
                    _ => {
                        if matches!(source, TrackSource::Remote { .. }) {
                            return automation_failure(
                                AutomationErrorCode::UnsupportedCommand,
                                "Remote auxiliary tracks are not yet supported",
                            );
                        }
                        if let Err(error) = self.add_track_source(&path) {
                            return automation_failure(AutomationErrorCode::InvalidCommand, error);
                        }
                        let track_id = self
                            .tracks
                            .tracks()
                            .iter()
                            .find(|track| !before.contains(&track.id))
                            .map(|track| track.id);
                        let Some(track_id) = track_id else {
                            return automation_failure(
                                AutomationErrorCode::Internal,
                                "the newly queued auxiliary track has no stable identity",
                            );
                        };
                        if let Some(name) = name
                            && let Some(track) = self
                                .tracks
                                .tracks_mut()
                                .iter_mut()
                                .find(|track| track.id == track_id)
                        {
                            track.name = name;
                        }
                        self.automation_auxiliary_open_state
                            .insert(track_id, AutomationAuxiliaryOpenState::Pending);
                        self.bump_workspace_revision();
                        AutomationExecution::Wait(AutomationWait::AuxiliaryTrack {
                            command_index,
                            track_id,
                            deadline: now + AUTOMATION_SOURCE_OPEN_TIMEOUT,
                        })
                    }
                }
            }
            AutomationCommand::RemoveTrack { track_id } => {
                let selected = BTreeSet::from([track_id]);
                match self.remove_metadata_selected_tracks(&selected) {
                    Ok(1) => {
                        self.bump_workspace_revision();
                        self.automation_alignment_handles.clear();
                        self.mark_retained_evidence_stale("the selected track set changed");
                        AutomationExecution::Complete {
                            message: format!("Removed track {}", track_id.get()),
                            data: serde_json::json!({"track_id": track_id.get()}),
                        }
                    }
                    Ok(_) => automation_failure(
                        AutomationErrorCode::ResourceNotFound,
                        format!("Track {} was not found", track_id.get()),
                    ),
                    Err(error) => automation_failure(AutomationErrorCode::InvalidState, error),
                }
            }
            AutomationCommand::Goto { loci } => match self.navigate_to_loci(&loci.join(" ")) {
                Ok(()) => {
                    self.bump_workspace_revision();
                    AutomationExecution::Complete {
                        message: format!("Navigated to {} ordered loci", loci.len()),
                        data: serde_json::json!({"loci": loci}),
                    }
                }
                Err(error) => automation_failure(AutomationErrorCode::InvalidCommand, error),
            },
            AutomationCommand::ChangeViewport {
                panel_id,
                operation,
            } => match self.apply_automation_viewport_operation(panel_id, operation) {
                Ok(data) => AutomationExecution::Complete {
                    message: format!("Changed panel {} viewport", panel_id.get()),
                    data,
                },
                Err(error) => automation_failure(AutomationErrorCode::InvalidCommand, error),
            },
            AutomationCommand::AddPanel { locus } => match self.add_view_panel(&locus) {
                Ok(panel_id) => {
                    self.bump_workspace_revision();
                    AutomationExecution::Complete {
                        message: format!("Added panel {} at {locus}", panel_id.get()),
                        data: serde_json::json!({"panel_id": panel_id.get(), "locus": locus}),
                    }
                }
                Err(error) => automation_failure(AutomationErrorCode::InvalidCommand, error),
            },
            AutomationCommand::ClosePanel { panel_id } => {
                if self.close_view_panel(panel_id) {
                    self.bump_workspace_revision();
                    AutomationExecution::Complete {
                        message: format!("Closed panel {}", panel_id.get()),
                        data: serde_json::json!({"panel_id": panel_id.get()}),
                    }
                } else {
                    automation_failure(
                        AutomationErrorCode::InvalidState,
                        "The panel was absent or was the only remaining panel",
                    )
                }
            }
            AutomationCommand::SetOption { scope, key, value } => {
                match self.apply_automation_option(&scope, &key, &value) {
                    Ok(()) => {
                        self.bump_workspace_revision();
                        AutomationExecution::Complete {
                            message: format!("Set option {key}"),
                            data: serde_json::json!({"key": key}),
                        }
                    }
                    Err(error) => automation_failure(AutomationErrorCode::InvalidCommand, error),
                }
            }
            AutomationCommand::Configure { setting } => {
                match self.apply_automation_setting(&setting) {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Applied a typed setting".to_owned(),
                        data,
                    },
                    Err(error) => automation_failure(AutomationErrorCode::SettingOutOfRange, error),
                }
            }
            AutomationCommand::Group {
                track_ids,
                mode,
                direction,
                tag,
            } => {
                let requested_mode = tag
                    .as_deref()
                    .and_then(|tag| parse_automation_sam_tag(tag).ok())
                    .map_or(mode, AlignmentGroupMode::Tag);
                let before = self.automation_alignment_group_state(&track_ids);
                match self.apply_automation_group(&track_ids, mode, direction, tag.as_deref()) {
                    Ok(()) => {
                        let effective = self.automation_alignment_group_state(&track_ids);
                        let changed = before != effective;
                        self.bump_workspace_revision();
                        AutomationExecution::Complete {
                            message: format!("Grouped {} alignment tracks", track_ids.len()),
                            data: serde_json::json!({
                                "requested": {"mode": requested_mode, "direction": direction},
                                "effective": effective,
                                "changed": changed,
                                "clamped": false,
                                "no_op": !changed,
                                "reason": serde_json::Value::Null,
                                "track_ids": track_ids.iter().map(|id| id.get()).collect::<Vec<_>>(),
                            }),
                        }
                    }
                    Err(error) => automation_failure(AutomationErrorCode::InvalidCommand, error),
                }
            }
            AutomationCommand::Sort {
                track_ids,
                mode,
                direction,
                tag,
            } => {
                let requested_mode = tag
                    .as_deref()
                    .and_then(|tag| parse_automation_sam_tag(tag).ok())
                    .map_or(mode, AlignmentSortMode::Tag);
                let before = self.automation_alignment_sort_state(&track_ids);
                match self.apply_automation_sort(&track_ids, mode, direction, tag.as_deref()) {
                    Ok(()) => {
                        let effective = self.automation_alignment_sort_state(&track_ids);
                        let changed = before != effective;
                        self.bump_workspace_revision();
                        AutomationExecution::Complete {
                            message: format!("Sorted {} alignment tracks", track_ids.len()),
                            data: serde_json::json!({
                                "requested": {"mode": requested_mode, "direction": direction},
                                "effective": effective,
                                "changed": changed,
                                "clamped": false,
                                "no_op": !changed,
                                "reason": serde_json::Value::Null,
                                "track_ids": track_ids.iter().map(|id| id.get()).collect::<Vec<_>>(),
                            }),
                        }
                    }
                    Err(error) => automation_failure(AutomationErrorCode::InvalidCommand, error),
                }
            }
            AutomationCommand::Filter {
                track_ids,
                expression,
            } => {
                let before = self.automation_alignment_filter_state(&track_ids);
                match self.apply_automation_filter(&track_ids, &expression) {
                    Ok(()) => {
                        let effective = self.automation_alignment_filter_state(&track_ids);
                        let changed = before != effective;
                        self.bump_workspace_revision();
                        AutomationExecution::Complete {
                            message: format!("Filtered {} alignment tracks", track_ids.len()),
                            data: serde_json::json!({
                                "requested": {"expression": expression},
                                "effective": effective,
                                "changed": changed,
                                "clamped": false,
                                "no_op": !changed,
                                "reason": serde_json::Value::Null,
                                "track_ids": track_ids.iter().map(|id| id.get()).collect::<Vec<_>>(),
                            }),
                        }
                    }
                    Err(error) => automation_failure(AutomationErrorCode::InvalidCommand, error),
                }
            }
            AutomationCommand::Select { selection } => {
                if let AutomationSelection::Position {
                    panel_id,
                    track_id,
                    position,
                    granularity: AutomationSelectionGranularity::ExactBase,
                } = selection
                    && !self.automation_exact_base_available(panel_id, track_id, position)
                {
                    if let Err(error) = self.apply_automation_viewport_operation(
                        panel_id,
                        AutomationViewportOperation::FitInterval {
                            start: position,
                            end: position.saturating_add(1),
                        },
                    ) {
                        return automation_failure(AutomationErrorCode::InvalidCommand, error);
                    }
                    return AutomationExecution::Wait(AutomationWait::ExactBaseSelection {
                        command_index,
                        panel_id,
                        track_id,
                        position,
                        previous_selection: None,
                        deadline: now + Duration::from_secs(30),
                    });
                }
                match self.apply_automation_selection(&selection) {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Updated semantic selection".to_owned(),
                        data,
                    },
                    Err(error) => automation_failure(AutomationErrorCode::InvalidCommand, error),
                }
            }
            AutomationCommand::FindAlignments {
                panel_id,
                track_id,
                selector,
                maximum_results,
            } => match self.find_automation_alignments(
                panel_id,
                track_id,
                &selector,
                maximum_results,
            ) {
                Ok(data) => AutomationExecution::Complete {
                    message: "Found bounded alignment matches".to_owned(),
                    data,
                },
                Err(error) => automation_failure(AutomationErrorCode::ResourceNotFound, error),
            },
            AutomationCommand::SelectAlignment { handle } => {
                match self.select_automation_alignment(&handle) {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Selected an opaque alignment handle".to_owned(),
                        data,
                    },
                    Err(error) => automation_failure(AutomationErrorCode::StaleHandle, error),
                }
            }
            AutomationCommand::DescribeRelatedAlignments => {
                match self.automation_related_alignments() {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Described related alignments".to_owned(),
                        data,
                    },
                    Err(error) => {
                        automation_failure(AutomationErrorCode::RelatedAlignmentUnavailable, error)
                    }
                }
            }
            AutomationCommand::NavigateRelatedAlignment {
                relationship,
                segment_index,
                destination,
            } => match self.navigate_automation_related_alignment(
                relationship,
                segment_index,
                destination,
            ) {
                Ok(data) => AutomationExecution::Complete {
                    message: "Navigated to a related alignment".to_owned(),
                    data,
                },
                Err(error) => {
                    automation_failure(AutomationErrorCode::RelatedAlignmentUnavailable, error)
                }
            },
            AutomationCommand::NavigateReviewItem { action } => {
                match self.apply_automation_review_navigation(&action) {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Navigated to a review item".to_owned(),
                        data,
                    },
                    Err(error) => {
                        automation_failure(AutomationErrorCode::ReviewItemNotFound, error)
                    }
                }
            }
            AutomationCommand::NavigateHistory { panel_id, action } => {
                match self.apply_automation_history_navigation(panel_id, action) {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Navigated through panel history".to_owned(),
                        data,
                    },
                    Err(error) => automation_failure(AutomationErrorCode::HistoryBoundary, error),
                }
            }
            AutomationCommand::WaitUntilSettled { timeout_millis } => {
                if self.automation_is_settled() {
                    AutomationExecution::Complete {
                        message: "Workspace is already settled".to_owned(),
                        data: serde_json::json!({"settled": true}),
                    }
                } else {
                    AutomationExecution::Wait(AutomationWait::Settled {
                        command_index,
                        deadline: now + Duration::from_millis(timeout_millis),
                    })
                }
            }
            AutomationCommand::Snapshot {
                path,
                scope,
                format,
            } => match self.start_automation_snapshot(
                command_index,
                PathBuf::from(path),
                scope,
                format,
            ) {
                Ok(()) => AutomationExecution::Wait(AutomationWait::File { command_index }),
                Err(error) => automation_failure(AutomationErrorCode::Io, error),
            },
            AutomationCommand::Export { path, kind } => {
                if kind == AutomationExportKind::SelectedEvidence {
                    match self.panels.active().retained_evidence.as_ref() {
                        None => {
                            return automation_failure(
                                AutomationErrorCode::NoSelection,
                                "no selected evidence is available to export".to_owned(),
                            );
                        }
                        Some(retained) if retained.selection.stale => {
                            return automation_failure(
                                AutomationErrorCode::SelectionStale,
                                "selected evidence is stale; refresh it explicitly before export"
                                    .to_owned(),
                            );
                        }
                        Some(_) => {}
                    }
                }
                match self.start_automation_export(command_index, PathBuf::from(path), kind) {
                    Ok(()) => AutomationExecution::Wait(AutomationWait::File { command_index }),
                    Err(error) => {
                        automation_failure(AutomationErrorCode::UnsupportedCommand, error)
                    }
                }
            }
            AutomationCommand::Save { path } => {
                match self.start_automation_session_save(command_index, PathBuf::from(path)) {
                    Ok(()) => AutomationExecution::Wait(AutomationWait::File { command_index }),
                    Err(error) => automation_failure(AutomationErrorCode::Io, error),
                }
            }
            AutomationCommand::Exit => AutomationExecution::Complete {
                message: "Exit requested after the automation report is durable".to_owned(),
                data: serde_json::json!({"exit": true}),
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn automation_workspace_snapshot(&self) -> AutomationWorkspaceSnapshot {
        let mut panels: Vec<_> = self
            .panels
            .iter()
            .map(|panel| {
                let interval = panel.viewport.interval();
                let maximum_scroll =
                    (panel.content_bottom_points - self.canvas_geometry.viewport.bottom()).max(0.0);
                let filters = self.primary_alignment_filters();
                let query = self
                    .alignment_tracks
                    .first()
                    .and_then(|track| track.projection(panel.id))
                    .and_then(|projection| projection.current_query.as_deref());
                let reads_rendered = filters.show_reads
                    && panel.viewport.detail_level() != DetailLevel::Overview
                    && query.is_some();
                let render_lanes = [
                    ("reference", filters.show_reference),
                    ("coverage", filters.show_coverage),
                    ("reads", filters.show_reads),
                    ("modifications", filters.show_modified_bases),
                ]
                .into_iter()
                .map(|(lane, configured)| {
                    let rendered = configured
                        && query.is_some()
                        && (lane != "reads" && lane != "modifications" || reads_rendered);
                    let omission_reason = (!rendered).then_some(if !configured {
                        AutomationRenderOmissionReason::Disabled
                    } else if query.is_none() {
                        if panel.pending_queries.alignment {
                            AutomationRenderOmissionReason::Loading
                        } else {
                            AutomationRenderOmissionReason::NoData
                        }
                    } else {
                        AutomationRenderOmissionReason::OverviewDetailThreshold
                    });
                    AutomationRenderLaneSnapshot {
                        lane: lane.to_owned(),
                        configured,
                        rendered,
                        omission_reason,
                        queried_items: query.map_or(0, |query| query.stats.records_eligible),
                        rendered_items: query.map_or(0, |query| query.stats.records_emitted),
                        sampled_items: query.map_or(0, |query| {
                            query
                                .stats
                                .records_eligible
                                .saturating_sub(query.stats.records_emitted)
                        }),
                        truncated: query.is_some_and(|query| query.stats.truncated),
                    }
                })
                .collect();
                AutomationPanelSnapshot {
                    id: panel.id,
                    locus: automation_locus(&panel.contig_name, interval),
                    contig: panel.contig_name.clone(),
                    start: interval.start(),
                    end: interval.end(),
                    detail_level: panel.viewport.detail_level(),
                    vertical_scroll_points: panel.vertical_scroll_points,
                    maximum_vertical_scroll_points: maximum_scroll,
                    render_generation: panel.generation_clock.current().get(),
                    settled: !panel.pending_queries.alignment
                        && !panel.pending_queries.reference
                        && !panel.pending_queries.tracks,
                    render_lanes,
                }
            })
            .collect();
        let mut tracks = self
            .alignment_tracks
            .iter()
            .map(|track| {
                let (lifecycle, failure) = match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => (AutomationTrackLifecycle::Opening, None),
                    AlignmentTrackLifecycle::Ready => (AutomationTrackLifecycle::Ready, None),
                    AlignmentTrackLifecycle::Failed(_) => (
                        AutomationTrackLifecycle::Failed,
                        Some("alignment track failed to open".to_owned()),
                    ),
                };
                AutomationTrackSnapshot {
                    id: track.config.id,
                    name: track.config.name.clone(),
                    kind: TrackKind::Alignment,
                    visible: track.config.visible,
                    collapsed: track.config.collapsed,
                    height: track.config.height,
                    lifecycle,
                    failure,
                    alignment: Some(AutomationAlignmentTrackSnapshot {
                        group_mode: track.config.filters.alignment_group_mode,
                        group_order: track.config.filters.alignment_group_order,
                        sort_mode: track.config.filters.alignment_sort_mode,
                        sort_order: track.config.filters.alignment_sort_order,
                        filter_expression: track.filter_editor.expression.clone(),
                        view_as_pairs: track.config.filters.view_as_pairs,
                        overlapping_mate_policy: track.config.filters.overlapping_mate_policy,
                        color_mode: track.config.filters.alignment_color_mode,
                        alignment_density: track.config.filters.alignment_density,
                        read_base_display_mode: track.config.filters.read_base_display_mode,
                        shade_read_bases_by_quality: track
                            .config
                            .filters
                            .shade_read_bases_by_quality,
                        show_modified_bases: track.config.filters.show_modified_bases,
                        modified_base_color_mode: track.config.filters.modified_base_color_mode,
                        show_reference: track.config.filters.show_reference,
                        show_coverage: track.config.filters.show_coverage,
                        show_reads: track.config.filters.show_reads,
                    }),
                }
            })
            .collect::<Vec<_>>();
        tracks.extend(self.tracks.tracks().iter().map(|track| {
            let (lifecycle, failure) = match self.automation_auxiliary_open_state.get(&track.id) {
                Some(AutomationAuxiliaryOpenState::Pending) => {
                    (AutomationTrackLifecycle::Opening, None)
                }
                Some(AutomationAuxiliaryOpenState::Failed(_)) => (
                    AutomationTrackLifecycle::Failed,
                    Some("auxiliary track failed to open".to_owned()),
                ),
                Some(AutomationAuxiliaryOpenState::Ready) | None => {
                    (AutomationTrackLifecycle::Ready, None)
                }
            };
            AutomationTrackSnapshot {
                id: track.id,
                name: track.name.clone(),
                kind: track.kind,
                visible: track.visible,
                collapsed: track.collapsed,
                height: track.height,
                lifecycle,
                failure,
                alignment: None,
            }
        }));
        let truncated = panels.len() > MAXIMUM_AUTOMATION_LOCI
            || tracks.len() > MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS;
        panels.truncate(MAXIMUM_AUTOMATION_LOCI);
        tracks.truncate(MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS);
        AutomationWorkspaceSnapshot {
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema_version: AUTOMATION_SCHEMA_VERSION,
            application_instance_id: self.application_instance_id.clone(),
            workspace_revision: self.workspace_revision,
            genome: AutomationGenomeSnapshot {
                name: self.assembly_name.clone(),
                reference_configured: self.reference_engine.is_some(),
                reference_ready: self.reference_engine.is_some()
                    && self.reference_dictionary.is_some(),
            },
            settled: self.automation_data_is_settled(),
            active_panel_id: self.panels.active().id,
            panels,
            tracks,
            selection: self.automation_selection_snapshot(),
            // Free-form status strings can contain local paths. Typed lifecycle
            // and readiness fields above provide safe status instead.
            status: Vec::new(),
            truncated,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn automation_selection_snapshot(&self) -> Option<AutomationSelectionSnapshot> {
        Some(observed_retained_selection(
            &self.panels.active().retained_evidence.as_ref()?.selection,
            self.panels.active().viewport.interval(),
        ))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn live_automation_selection_snapshot(&self) -> Option<AutomationSelectionSnapshot> {
        const MAXIMUM_FIELD_KEY_BYTES: usize = 256;
        const MAXIMUM_FIELD_VALUE_BYTES: usize = 4_096;

        let panel = self.panels.active();
        let bounded_fields = |fields: Vec<(String, String)>| {
            let truncated = fields.len() > MAXIMUM_AUTOMATION_EVIDENCE_FIELDS;
            let fields = fields
                .into_iter()
                .take(MAXIMUM_AUTOMATION_EVIDENCE_FIELDS)
                .map(|(key, value)| AutomationEvidenceField {
                    key: bounded_automation_text(&key, MAXIMUM_FIELD_KEY_BYTES),
                    value: bounded_automation_text(&value, MAXIMUM_FIELD_VALUE_BYTES),
                })
                .collect::<Vec<_>>();
            (fields, truncated)
        };
        let snapshot = |track_id, kind, position, start, end, raw_fields: Vec<(String, String)>| {
            let (fields, truncated) = bounded_fields(raw_fields);
            AutomationSelectionSnapshot {
                selection_id: String::new(),
                evidence_revision: 0,
                workspace_revision_at_selection: self.workspace_revision,
                panel_id: panel.id,
                track_id,
                kind,
                granularity: None,
                anchor_position: position,
                evidence_start: start,
                evidence_end: end,
                position,
                start,
                end,
                fields,
                provenance: Vec::new(),
                visible_in_active_view: true,
                stale: false,
                stale_reason: None,
                truncated,
            }
        };

        if let Some(selection) = panel.selected_variant
            && let Some(inspector) = self.variant_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Variant,
                Some(record.start),
                Some(record.start),
                Some(record.end),
                variant_inspector_fields(&inspector, record),
            ));
        }
        if let Some(selection) = panel.selected_feature
            && let Some(inspector) = self.feature_inspector_snapshot()
        {
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Feature,
                Some(inspector.record.start),
                Some(inspector.record.start),
                Some(inspector.record.end),
                feature_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_gwas
            && let Some(inspector) = self.gwas_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Gwas,
                Some(record.position),
                Some(record.position),
                Some(record.position.saturating_add(1)),
                gwas_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_quantitative
            && let Some(inspector) = self.quantitative_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Quantitative,
                Some(selection.position),
                Some(record.start),
                Some(record.end),
                quantitative_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_copy_number
            && let Some(inspector) = self.copy_number_inspector_snapshot()
        {
            let segment = &inspector.query.segments[inspector.segment_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::CopyNumber,
                Some(segment.start),
                Some(segment.start),
                Some(segment.end),
                copy_number_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_mutation
            && let Some(inspector) = self.mutation_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Mutation,
                Some(record.start),
                Some(record.start),
                Some(record.end),
                mutation_inspector_fields(&inspector),
            ));
        }
        if let Some(selection) = panel.selected_interaction
            && let Some(inspector) = self.interaction_inspector_snapshot()
        {
            let record = &inspector.query.records[inspector.record_index];
            return Some(snapshot(
                Some(selection.track_id),
                AutomationSelectionKind::Interaction,
                Some(record.source.start),
                Some(record.source.start),
                Some(record.source.end),
                interaction_inspector_fields(&inspector),
            ));
        }

        let alignment_track = panel
            .selected_alignment_track
            .or(match panel.selected_track {
                Some(TrackFrameKey::Alignment(id)) => Some(id),
                _ => None,
            });
        if let Some(position) = panel.selected_pileup_position
            && let Some(inspector) = self.pileup_inspector_snapshot()
        {
            let query =
                alignment_track.and_then(|track_id| self.alignment_query_for_track(track_id));
            let interval = query.and_then(|query| {
                let (index, _) = query.pileup.bin_at(query.interval, position)?;
                query.pileup.bin_interval(query.interval, index)
            });
            let mut captured = snapshot(
                alignment_track,
                AutomationSelectionKind::Pileup,
                Some(position),
                interval.map(GenomicInterval::start).or(Some(position)),
                interval
                    .map(GenomicInterval::end)
                    .or(Some(position.saturating_add(1))),
                inspector.fields,
            );
            if let Some(query) = query {
                captured.provenance = vec![
                    AutomationEvidenceField {
                        key: "bin_width_bases".to_owned(),
                        value: interval.map_or(1, GenomicInterval::len).to_string(),
                    },
                    AutomationEvidenceField {
                        key: "eligible_reads".to_owned(),
                        value: query.pileup.provenance.eligible_records.to_string(),
                    },
                    AutomationEvidenceField {
                        key: "rendered_reads".to_owned(),
                        value: query.pileup.provenance.rendered_records.to_string(),
                    },
                    AutomationEvidenceField {
                        key: "all_eligible_reads".to_owned(),
                        value: query.pileup.provenance.all_eligible_records.to_string(),
                    },
                    AutomationEvidenceField {
                        key: "overlapping_mate_policy".to_owned(),
                        value: format!("{:?}", query.pileup.provenance.overlapping_mate_policy)
                            .to_ascii_lowercase(),
                    },
                ];
            }
            return Some(captured);
        }
        if let Some(position) = panel.selected_modification_position
            && let Some(inspector) = self.modification_inspector_snapshot()
        {
            return Some(snapshot(
                alignment_track,
                AutomationSelectionKind::Modification,
                Some(position),
                Some(position),
                Some(position.saturating_add(1)),
                inspector.fields,
            ));
        }
        if let Some((start, end)) = panel.selected_junction
            && let Some(inspector) = self.junction_inspector_snapshot()
        {
            return Some(snapshot(
                alignment_track,
                AutomationSelectionKind::Junction,
                Some(start),
                Some(start),
                Some(end),
                inspector.fields,
            ));
        }
        if let Some(index) = panel.selected_record
            && let Some(inspector) = self.inspector_snapshot()
        {
            let track_id = alignment_track.unwrap_or_else(|| self.primary_alignment_track_id());
            let query = self.alignment_query_for_track(track_id)?;
            let record = query.materialize_record(index).ok()?;
            let fields = inspector
                .fields
                .into_iter()
                .filter(|(key, _)| automation_read_evidence_field_is_public(key))
                .collect();
            return Some(snapshot(
                Some(track_id),
                if panel.selected_read_event.is_some() {
                    AutomationSelectionKind::ReadEvent
                } else {
                    AutomationSelectionKind::Read
                },
                Some(record.start),
                Some(record.start),
                Some(record.end),
                fields,
            ));
        }
        match panel.selected_track {
            Some(TrackFrameKey::Alignment(id) | TrackFrameKey::Auxiliary(id)) => Some(snapshot(
                Some(id),
                AutomationSelectionKind::Track,
                None,
                None,
                None,
                Vec::new(),
            )),
            Some(TrackFrameKey::Reference) => Some(snapshot(
                None,
                AutomationSelectionKind::Track,
                None,
                None,
                None,
                Vec::new(),
            )),
            None => None,
        }
    }

    pub(crate) fn bump_workspace_revision(&mut self) -> u64 {
        self.workspace_revision = self.workspace_revision.saturating_add(1);
        self.workspace_revision
    }

    pub(crate) fn mark_retained_evidence_stale(&mut self, reason: &str) {
        for panel in self.panels.iter_mut() {
            if let Some(retained) = &mut panel.retained_evidence {
                retained.selection.stale = true;
                retained.selection.stale_reason = Some(bounded_automation_text(reason, 512));
            }
        }
    }

    pub(crate) fn retain_active_selection(
        &mut self,
        granularity: Option<AutomationSelectionGranularity>,
    ) {
        let Some(mut selection) = self.live_automation_selection_snapshot() else {
            self.panels.active_mut().retained_evidence = None;
            return;
        };
        selection.selection_id = uuid::Uuid::new_v4().to_string();
        selection.evidence_revision = self.next_evidence_revision;
        selection.workspace_revision_at_selection = self.workspace_revision;
        selection.granularity = granularity;
        if let Some(track_id) = selection.track_id
            && let Some(generation) = self
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == track_id)
                .and_then(|track| track.projection(selection.panel_id))
                .map(|projection| projection.clock.current().get())
        {
            selection.provenance.push(AutomationEvidenceField {
                key: "query_generation".to_owned(),
                value: generation.to_string(),
            });
        }
        self.next_evidence_revision = self.next_evidence_revision.saturating_add(1);
        let inspector = match selection.kind {
            AutomationSelectionKind::Read | AutomationSelectionKind::ReadEvent => {
                self.inspector_snapshot()
            }
            AutomationSelectionKind::Pileup => self.pileup_inspector_snapshot(),
            AutomationSelectionKind::Modification => self.modification_inspector_snapshot(),
            AutomationSelectionKind::Junction => self.junction_inspector_snapshot(),
            _ => None,
        };
        let export = inspector.as_ref().map_or_else(
            || {
                if selection.fields.is_empty() {
                    String::new()
                } else {
                    automation_inspector_export(&InspectorSnapshot {
                        fields: selection
                            .fields
                            .iter()
                            .map(|field| (field.key.clone(), field.value.clone()))
                            .collect(),
                        auxiliary: Vec::new(),
                        event_sequence: None,
                        cigar: None,
                        read_sequence: None,
                    })
                }
            },
            automation_inspector_export,
        );
        self.panels.active_mut().retained_evidence = Some(RetainedEvidenceSnapshot {
            selection,
            inspector,
            export,
        });
    }

    pub(crate) fn apply_automation_viewport_operation(
        &mut self,
        panel_id: PanelId,
        operation: AutomationViewportOperation,
    ) -> Result<serde_json::Value> {
        anyhow::ensure!(
            self.panels.activate(panel_id),
            "panel {} was not found",
            panel_id.get()
        );
        let panel = self.panels.active();
        let current = panel.viewport;
        let contig_length = panel.contig_length;
        let next = automation_viewport_after_operation(current, contig_length, operation)?;
        self.panels.active_mut().viewport = next;
        self.panels
            .active_mut()
            .navigation_history
            .visit(next.interval());
        self.viewport_changed();
        self.bump_workspace_revision();
        let interval = next.interval();
        Ok(serde_json::json!({
            "panel_id": panel_id.get(),
            "locus": automation_locus(&self.panels.active().contig_name, interval),
            "contig": self.panels.active().contig_name,
            "start": interval.start(),
            "end": interval.end(),
            "detail_level": next.detail_level(),
            "workspace_revision": self.workspace_revision,
        }))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_automation_option(
        &mut self,
        scope: &AutomationOptionScope,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<()> {
        match *scope {
            AutomationOptionScope::Workspace => {
                let mut query_changed = false;
                match key {
                    "show_reference" => {
                        self.primary_alignment_filters_mut().show_reference =
                            automation_bool(value, key)?;
                    }
                    "show_coverage" => {
                        self.primary_alignment_filters_mut().show_coverage =
                            automation_bool(value, key)?;
                    }
                    "show_reads" => {
                        self.primary_alignment_filters_mut().show_reads =
                            automation_bool(value, key)?;
                    }
                    "show_junctions" => {
                        self.primary_alignment_filters_mut().show_junctions =
                            automation_bool(value, key)?;
                    }
                    "show_junction_labels" => {
                        self.primary_alignment_filters_mut().show_junction_labels =
                            automation_bool(value, key)?;
                    }
                    "show_coverage_center_line" => {
                        self.primary_alignment_filters_mut()
                            .show_coverage_center_line = automation_bool(value, key)?;
                    }
                    "view_as_pairs" => {
                        self.primary_alignment_filters_mut().view_as_pairs =
                            automation_bool(value, key)?;
                    }
                    "quality_weighted_alleles" => {
                        self.primary_alignment_filters_mut()
                            .quality_weighted_alleles = automation_bool(value, key)?;
                    }
                    "shade_read_bases_by_quality" => {
                        self.primary_alignment_filters_mut()
                            .shade_read_bases_by_quality = automation_bool(value, key)?;
                    }
                    "show_modified_bases" => {
                        self.primary_alignment_filters_mut().show_modified_bases =
                            automation_bool(value, key)?;
                    }
                    "show_modification_frequency" => {
                        let requested = automation_bool(value, key)?;
                        query_changed = requested
                            != self.primary_alignment_filters().show_modification_frequency;
                        self.primary_alignment_filters_mut()
                            .show_modification_frequency = requested;
                    }
                    "minimum_mapping_quality" => {
                        let requested = automation_optional_u64(value, key)?
                            .map(|value| automation_u8(value, key))
                            .transpose()?;
                        query_changed = requested
                            != self
                                .primary_active_filters()
                                .alignment
                                .minimum_mapping_quality;
                        self.primary_active_filters_mut()
                            .alignment
                            .minimum_mapping_quality = requested;
                        self.primary_filter_editor_mut().minimum_mapq =
                            requested.unwrap_or_default();
                    }
                    "minimum_base_quality" => {
                        self.primary_alignment_filters_mut().minimum_base_quality =
                            automation_u8(automation_u64(value, key)?, key)?;
                    }
                    "minimum_allele_fraction" => {
                        let fraction = automation_f32(value, key)?;
                        anyhow::ensure!(
                            (0.0..=1.0).contains(&fraction),
                            "{key} must be between 0 and 1"
                        );
                        self.primary_alignment_filters_mut().minimum_allele_fraction = fraction;
                    }
                    "vertical_density" => {
                        let density = automation_f32(value, key)?;
                        anyhow::ensure!(
                            (MIN_VERTICAL_DENSITY..=MAX_VERTICAL_DENSITY).contains(&density),
                            "{key} must be between {MIN_VERTICAL_DENSITY} and {MAX_VERTICAL_DENSITY}"
                        );
                        self.primary_alignment_filters_mut().alignment_density = density;
                    }
                    "coverage_scale_maximum" => {
                        let maximum = automation_optional_u64(value, key)?
                            .map(|value| automation_u32(value, key))
                            .transpose()?;
                        anyhow::ensure!(maximum != Some(0), "{key} must be positive or null");
                        self.primary_alignment_filters_mut().coverage_scale_maximum = maximum;
                    }
                    "minimum_indel_size" => {
                        let minimum = automation_u32(automation_u64(value, key)?, key)?;
                        anyhow::ensure!(minimum > 0, "{key} must be positive");
                        self.primary_alignment_filters_mut().minimum_indel_size = minimum;
                    }
                    "minimum_event_label_size" => {
                        let minimum = automation_u32(automation_u64(value, key)?, key)?;
                        anyhow::ensure!(minimum > 0, "{key} must be positive");
                        self.primary_alignment_filters_mut()
                            .minimum_event_label_size = minimum;
                    }
                    _ => anyhow::bail!("unknown workspace option {key}"),
                }
                self.invalidate_automation_alignment_layouts(None, query_changed);
                if query_changed {
                    self.request_all_panel_queries();
                }
                self.rebuild_all_panel_displays();
            }
            AutomationOptionScope::Track { track_id } => {
                let primary_id = self.primary_alignment_track_id();
                let mut query_changed = false;
                if let Some(track) = self
                    .alignment_tracks
                    .iter_mut()
                    .find(|track| track.config.id == track_id)
                {
                    match key {
                        "visible" => {
                            let requested = automation_bool(value, key)?;
                            query_changed = requested != track.config.visible;
                            track.config.visible = requested;
                        }
                        "collapsed" => track.config.collapsed = automation_bool(value, key)?,
                        "height" => {
                            let height = automation_f32(value, key)?;
                            anyhow::ensure!(
                                (40.0..=1_200.0).contains(&height),
                                "alignment-track height must be between 40 and 1200"
                            );
                            track.config.height = height;
                        }
                        "name" => track.config.name = automation_name(value, key)?,
                        "sample_key" => {
                            track.config.sample_key = automation_optional_name(value, key)?;
                        }
                        _ => anyhow::bail!("unknown alignment-track option {key}"),
                    }
                    if track_id == primary_id {
                        self.activate_first_alignment_track()?;
                    }
                    self.invalidate_automation_alignment_layouts(Some(&[track_id]), query_changed);
                    if query_changed {
                        self.request_all_alignment_queries();
                    }
                    self.rebuild_all_panel_displays();
                } else if let Some(track) = self
                    .tracks
                    .tracks_mut()
                    .iter_mut()
                    .find(|track| track.id == track_id)
                {
                    match key {
                        "visible" => {
                            let requested = automation_bool(value, key)?;
                            query_changed = requested != track.visible;
                            track.visible = requested;
                        }
                        "collapsed" => track.collapsed = automation_bool(value, key)?,
                        "height" => {
                            anyhow::ensure!(
                                !matches!(track.kind, TrackKind::Mutation | TrackKind::CopyNumber),
                                "{:?} track height is derived from its semantic rows",
                                track.kind
                            );
                            let height = automation_f32(value, key)?;
                            anyhow::ensure!(
                                (40.0..=600.0).contains(&height),
                                "track height must be between 40 and 600"
                            );
                            track.height = height;
                        }
                        "name" => track.name = automation_name(value, key)?,
                        "sample_key" => {
                            track.sample_key = automation_optional_name(value, key)?;
                        }
                        _ => anyhow::bail!("unknown auxiliary-track option {key}"),
                    }
                    if query_changed {
                        self.invalidate_and_request_all_auxiliary_track_queries();
                    }
                    self.rebuild_all_panel_displays();
                } else {
                    anyhow::bail!("track {} was not found", track_id.get());
                }
            }
            AutomationOptionScope::Panel { panel_id } => {
                let Some(panel) = self.panels.get_mut(panel_id) else {
                    anyhow::bail!("panel {} was not found", panel_id.get());
                };
                match key {
                    "vertical_scroll_points" => {
                        let scroll = automation_f32(value, key)?;
                        anyhow::ensure!(scroll >= 0.0, "{key} must be non-negative");
                        panel.vertical_scroll_points =
                            scroll.min(panel.content_bottom_points.max(0.0));
                    }
                    _ => anyhow::bail!("unknown panel option {key}"),
                }
                self.rebuild_all_panel_displays();
            }
        }
        self.display_dirty = true;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_automation_setting(
        &mut self,
        setting: &AutomationSetting,
    ) -> Result<serde_json::Value> {
        let requested = serde_json::to_value(setting)?;
        let before = serde_json::to_value(self.automation_workspace_snapshot())?;
        match setting {
            AutomationSetting::ShowReference { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_reference",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ShowCoverage { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_coverage",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ShowReads { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_reads",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ShowJunctions { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_junctions",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ViewAsPairs { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "view_as_pairs",
                &serde_json::json!(value),
            )?,
            AutomationSetting::AlignmentDensity { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "vertical_density",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ReadBaseDisplayMode { value } => {
                self.primary_alignment_filters_mut().read_base_display_mode = *value;
                self.invalidate_automation_alignment_layouts(None, false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ShadeReadBasesByQuality { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "shade_read_bases_by_quality",
                &serde_json::json!(value),
            )?,
            AutomationSetting::ShowModifiedBases { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "show_modified_bases",
                &serde_json::json!(value),
            )?,
            AutomationSetting::OverlappingMatePolicy { value } => {
                let changed = self.primary_alignment_filters().overlapping_mate_policy != *value;
                self.primary_alignment_filters_mut().overlapping_mate_policy = *value;
                self.invalidate_automation_alignment_layouts(None, changed);
                if changed {
                    self.request_all_panel_queries();
                }
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ModifiedBaseColorMode { value } => {
                self.primary_alignment_filters_mut()
                    .modified_base_color_mode = *value;
                self.invalidate_automation_alignment_layouts(None, false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::AlignmentColorMode { track_ids, value } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    track.config.filters.alignment_color_mode = *value;
                }
                self.invalidate_automation_alignment_layouts(Some(track_ids), false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::TrackHeight { track_id, value } => self.apply_automation_option(
                &AutomationOptionScope::Track {
                    track_id: *track_id,
                },
                "height",
                &serde_json::json!(value),
            )?,
            AutomationSetting::TrackCollapsed { track_id, value } => self.apply_automation_option(
                &AutomationOptionScope::Track {
                    track_id: *track_id,
                },
                "collapsed",
                &serde_json::json!(value),
            )?,
            AutomationSetting::TrackVisible { track_id, value } => self.apply_automation_option(
                &AutomationOptionScope::Track {
                    track_id: *track_id,
                },
                "visible",
                &serde_json::json!(value),
            )?,
            AutomationSetting::VerticalScrollPoints { panel_id, value } => self
                .apply_automation_option(
                    &AutomationOptionScope::Panel {
                        panel_id: *panel_id,
                    },
                    "vertical_scroll_points",
                    &serde_json::json!(value),
                )?,
        }
        self.bump_workspace_revision();
        let workspace = self.automation_workspace_snapshot();
        let after = serde_json::to_value(&workspace)?;
        let effective = match setting {
            AutomationSetting::VerticalScrollPoints { panel_id, .. } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| serde_json::json!(panel.vertical_scroll_points)),
            AutomationSetting::TrackHeight { track_id, .. } => workspace
                .tracks
                .iter()
                .find(|track| track.id == *track_id)
                .map(|track| serde_json::json!(track.height)),
            _ => Some(
                requested
                    .get("value")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            ),
        }
        .unwrap_or(serde_json::Value::Null);
        let requested_value = requested
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let clamped = requested_value != serde_json::Value::Null && requested_value != effective;
        Ok(serde_json::json!({
            "setting": requested.get("setting").cloned().unwrap_or(serde_json::Value::Null),
            "requested": requested_value,
            "effective": effective,
            "changed": before != after,
            "clamped": clamped,
            "reason": clamped.then_some("requested value was constrained by the rendered layout"),
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
        }))
    }

    pub(crate) fn automation_review_queue_snapshot(&self) -> serde_json::Value {
        let active = self.panels.active();
        let mut items = self
            .review_items
            .iter()
            .take(MAXIMUM_REVIEW_ITEMS)
            .map(|item| {
                serde_json::json!({
                    "id": item.id,
                    "label": bounded_automation_text(&item.label, 4_096),
                    "locus": format!("{}:{}-{}", item.locus.contig, item.locus.start.saturating_add(1), item.locus.end),
                    "contig": item.locus.contig,
                    "start": item.locus.start,
                    "end": item.locus.end,
                    "status": item.status,
                    "current": active.contig_name == item.locus.contig
                        && active.viewport.interval().start() == item.locus.start
                        && active.viewport.interval().end() == item.locus.end,
                })
            })
            .collect::<Vec<_>>();
        let truncated = self.review_items.len() > items.len();
        items.shrink_to_fit();
        serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "items": items,
            "truncated": truncated,
        })
    }

    pub(crate) fn automation_navigation_history_snapshot(
        &self,
        panel_id: PanelId,
    ) -> Result<serde_json::Value> {
        let panel = self
            .panels
            .get(panel_id)
            .with_context(|| format!("panel {} was not found", panel_id.get()))?;
        let entries = panel
            .navigation_history
            .entries()
            .enumerate()
            .take(MAXIMUM_AUTOMATION_HISTORY_ENTRIES)
            .map(|(index, interval)| {
                let contig = self
                    .assembly
                    .dictionary
                    .get(interval.contig())
                    .map_or(panel.contig_name.as_str(), |contig| contig.name());
                serde_json::json!({
                    "index": index,
                    "locus": automation_locus(contig, *interval),
                    "contig": contig,
                    "start": interval.start(),
                    "end": interval.end(),
                    "current": index == panel.navigation_history.cursor(),
                })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "panel_id": panel_id.get(),
            "entries": entries,
            "cursor": panel.navigation_history.cursor(),
            "can_go_back": panel.navigation_history.can_go_back(),
            "can_go_forward": panel.navigation_history.can_go_forward(),
            "truncated": panel.navigation_history.len() > MAXIMUM_AUTOMATION_HISTORY_ENTRIES,
        }))
    }

    pub(crate) fn apply_automation_review_navigation(
        &mut self,
        action: &AutomationReviewNavigation,
    ) -> Result<serde_json::Value> {
        let item = match action {
            AutomationReviewNavigation::Item { review_item_id } => self
                .review_items
                .iter()
                .find(|item| item.id == *review_item_id),
            AutomationReviewNavigation::NextPending => self
                .review_items
                .iter()
                .find(|item| item.status == ReviewStatus::Pending),
        }
        .cloned()
        .context("review item was not found")?;
        let locus = format!(
            "{}:{}-{}",
            item.locus.contig,
            item.locus.start.saturating_add(1),
            item.locus.end
        );
        self.navigate_to_locus(&locus)?;
        self.bump_workspace_revision();
        Ok(serde_json::json!({
            "review_item_id": item.id,
            "locus": locus,
            "start": item.locus.start,
            "end": item.locus.end,
            "settled": self.automation_is_settled(),
            "workspace_revision": self.workspace_revision,
        }))
    }

    pub(crate) fn apply_automation_history_navigation(
        &mut self,
        panel_id: PanelId,
        action: AutomationHistoryNavigation,
    ) -> Result<serde_json::Value> {
        anyhow::ensure!(
            self.panels.activate(panel_id),
            "panel {} was not found",
            panel_id.get()
        );
        let interval = match action {
            AutomationHistoryNavigation::Back => self.panels.active_mut().navigation_history.back(),
            AutomationHistoryNavigation::Forward => {
                self.panels.active_mut().navigation_history.forward()
            }
            AutomationHistoryNavigation::Index { index } => {
                self.panels.active_mut().navigation_history.select(index)
            }
        }
        .context("navigation history boundary was reached")?;
        self.navigate_to_history_interval(interval);
        self.bump_workspace_revision();
        Ok(serde_json::json!({
            "panel_id": panel_id.get(),
            "start": interval.start(),
            "end": interval.end(),
            "settled": self.automation_is_settled(),
            "workspace_revision": self.workspace_revision,
        }))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn find_automation_alignments(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        selector: &AutomationAlignmentSelector,
        maximum_results: usize,
    ) -> Result<serde_json::Value> {
        let track = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .with_context(|| format!("alignment track {} was not found", track_id.get()))?;
        let projection = track
            .projection(panel_id)
            .context("the alignment query is unavailable for this panel")?;
        let generation = projection.clock.current().get();
        let query = Arc::clone(
            projection
                .current_query
                .as_ref()
                .context("the alignment query has not settled")?,
        );
        let filters = track.config.filters.clone();
        let mut read_name_counts = HashMap::<Vec<u8>, usize>::new();
        let mut supplementary_names = HashSet::<Vec<u8>>::new();
        for index in 0..query.batch.len() {
            let Some(read_name) = query.batch.read_name(index) else {
                continue;
            };
            *read_name_counts.entry(read_name.to_vec()).or_default() += 1;
            if query.batch.flags[index].is_supplementary() {
                supplementary_names.insert(read_name.to_vec());
            }
        }
        self.automation_alignment_handles.clear();
        let mut hits = Vec::new();
        let mut matched_count = 0usize;
        for index in 0..query.batch.len() {
            let record = query.materialize_record(index)?;
            let pair = pair_evidence(
                &record,
                query.interval,
                TemplateLengthBounds {
                    minimum: filters.minimum_template_length,
                    maximum: filters.maximum_template_length,
                },
            );
            let chain = selected_supplementary_chain(&record, "");
            let query_chain_segments = read_name_counts
                .get(&record.read_name)
                .copied()
                .filter(|count| *count > 1 && supplementary_names.contains(&record.read_name))
                .unwrap_or_default();
            let has_navigable_chain = chain.as_ref().is_some_and(|chain| chain.segments.len() > 1)
                || query_chain_segments > 1;
            let modifications = decode_modified_bases(&record).unwrap_or_default();
            let is_match = match selector {
                AutomationAlignmentSelector::ExactReadName { read_name } => {
                    record.read_name == read_name.as_bytes()
                }
                AutomationAlignmentSelector::Position { position } => {
                    record.start <= *position && *position < record.end
                }
                AutomationAlignmentSelector::Interval { start, end } => {
                    record.start < *end && *start < record.end
                }
                AutomationAlignmentSelector::Semantic {
                    alignment_state,
                    pair_class,
                    has_mapped_mate,
                    has_supplementary_chain,
                    has_modified_bases,
                } => {
                    let state_matches = alignment_state.is_none_or(|state| match state {
                        AutomationAlignmentState::Primary => {
                            !record.flags.is_secondary() && !record.flags.is_supplementary()
                        }
                        AutomationAlignmentState::Secondary => record.flags.is_secondary(),
                        AutomationAlignmentState::Supplementary => record.flags.is_supplementary(),
                    });
                    let mate_available = record.mate_contig.is_some()
                        && record.mate_start.is_some()
                        && !record.flags.is_mate_unmapped();
                    state_matches
                        && pair_class.as_ref().is_none_or(|expected| {
                            format!("{:?}", pair.classification).eq_ignore_ascii_case(expected)
                        })
                        && has_mapped_mate.is_none_or(|expected| expected == mate_available)
                        && has_supplementary_chain
                            .is_none_or(|expected| expected == has_navigable_chain)
                        && has_modified_bases
                            .is_none_or(|expected| expected != modifications.is_empty())
                }
            };
            if !is_match {
                continue;
            }
            matched_count = matched_count.saturating_add(1);
            if hits.len() >= maximum_results {
                continue;
            }
            let handle = uuid::Uuid::new_v4().to_string();
            self.automation_alignment_handles.insert(
                handle.clone(),
                AutomationAlignmentHandleTarget {
                    panel_id,
                    track_id,
                    query_generation: generation,
                    record_index: index,
                },
            );
            hits.push(serde_json::json!({
                "handle": handle,
                "panel_id": panel_id.get(),
                "track_id": track_id.get(),
                "start": record.start,
                "end": record.end,
                "flags": record.flags.bits(),
                "mapping_quality": record.mapping_quality,
                "pair_class": format!("{:?}", pair.classification).to_ascii_lowercase(),
                "mapped_mate_available": record.mate_contig.is_some()
                    && record.mate_start.is_some()
                    && !record.flags.is_mate_unmapped(),
                "supplementary_segment_count": chain
                    .as_ref()
                    .map_or(0, |chain| chain.segments.len())
                    .max(query_chain_segments),
                "modification_code_count": modifications
                    .iter()
                    .map(|call| call.code)
                    .collect::<HashSet<_>>()
                    .len(),
            }));
        }
        Ok(serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "query_generation": generation,
            "hits": hits,
            "truncated": matched_count > maximum_results,
        }))
    }

    pub(crate) fn select_automation_alignment(
        &mut self,
        handle: &str,
    ) -> Result<serde_json::Value> {
        let target = self
            .automation_alignment_handles
            .get(handle)
            .copied()
            .context("alignment handle is stale or unknown")?;
        let query = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == target.track_id)
            .and_then(|track| track.projection(target.panel_id))
            .filter(|projection| projection.clock.current().get() == target.query_generation)
            .and_then(|projection| projection.current_query.as_ref())
            .cloned()
            .context("alignment handle is stale")?;
        let record = query
            .materialize_record(target.record_index)
            .context("alignment handle no longer identifies a record")?;
        anyhow::ensure!(
            self.panels.activate(target.panel_id),
            "alignment panel is unavailable"
        );
        let contig_name = self.panels.active().contig_name.clone();
        let chain = automation_query_supplementary_chain(&query, &record, &contig_name)?;
        self.panels.active_mut().clear_selection();
        self.panels.active_mut().selected_alignment_track = Some(target.track_id);
        self.panels.active_mut().selected_alignment_tracks = vec![target.track_id];
        self.panels.active_mut().selected_track = Some(TrackFrameKey::Alignment(target.track_id));
        self.panels.active_mut().selected_record = Some(target.record_index);
        self.panels.active_mut().selected_chain = chain;
        self.bump_workspace_revision();
        self.retain_active_selection(None);
        self.rebuild_genomic_display();
        Ok(serde_json::json!({
            "selection": self.automation_selection_snapshot(),
            "workspace_revision": self.workspace_revision,
        }))
    }

    pub(crate) fn automation_related_alignments(&self) -> Result<serde_json::Value> {
        let track_id = self
            .panels
            .active()
            .selected_alignment_track
            .context("no alignment is selected")?;
        let index = self
            .panels
            .active()
            .selected_record
            .context("no alignment is selected")?;
        let record = self
            .alignment_query_for_track(track_id)
            .context("selected alignment query is unavailable")?
            .materialize_record(index)?;
        let segments =
            self.panels
                .active()
                .selected_chain
                .as_ref()
                .map_or_else(Vec::new, |chain| {
                    chain
                        .segments
                        .iter()
                        .enumerate()
                        .map(|(index, segment)| {
                            serde_json::json!({
                                "segment_index": index,
                                "contig": segment.reference_name,
                                "start": segment.start,
                                "end": segment.end,
                                "current": index == chain.active_segment,
                            })
                        })
                        .collect()
                });
        Ok(serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "mate_available": record.mate_contig.is_some()
                && record.mate_start.is_some()
                && !record.flags.is_mate_unmapped(),
            "supplementary_segments": segments,
        }))
    }

    pub(crate) fn navigate_automation_related_alignment(
        &mut self,
        relationship: AutomationAlignmentRelationship,
        segment_index: Option<usize>,
        destination: AutomationNavigationDestination,
    ) -> Result<serde_json::Value> {
        let panel_id = match (relationship, destination) {
            (AutomationAlignmentRelationship::Mate, AutomationNavigationDestination::NewPanel) => {
                self.open_selected_mate_in_new_panel()?
            }
            (
                AutomationAlignmentRelationship::Mate,
                AutomationNavigationDestination::CurrentPanel,
            ) => {
                let (track_id, read_name, locus) = self
                    .selected_mate_target()
                    .context("the selected alignment has no mapped mate")?;
                self.navigate_to_locus(&locus)?;
                let panel = self.panels.active_mut();
                panel.pending_read_selection = Some((track_id, read_name));
                panel.selected_alignment_track = Some(track_id);
                panel.id
            }
            (
                AutomationAlignmentRelationship::SupplementarySegment,
                AutomationNavigationDestination::NewPanel,
            ) => self.open_chain_segment_in_new_panel(
                segment_index.context("supplementary segment index is required")?,
            )?,
            (
                AutomationAlignmentRelationship::SupplementarySegment,
                AutomationNavigationDestination::CurrentPanel,
            ) => {
                self.navigate_to_chain_segment(
                    segment_index.context("supplementary segment index is required")?,
                )?;
                self.panels.active().id
            }
        };
        self.bump_workspace_revision();
        Ok(serde_json::json!({
            "panel_id": panel_id.get(),
            "workspace_revision": self.workspace_revision,
            "settled": self.automation_is_settled(),
        }))
    }

    pub(crate) fn apply_automation_group(
        &mut self,
        track_ids: &[TrackId],
        mode: AlignmentGroupMode,
        direction: AlignmentOrderDirection,
        tag: Option<&str>,
    ) -> Result<()> {
        self.ensure_automation_alignment_tracks(track_ids)?;
        let mode = tag.map_or(Ok(mode), |tag| {
            parse_automation_sam_tag(tag).map(AlignmentGroupMode::Tag)
        })?;
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.contains(&track.config.id))
        {
            track.config.filters.alignment_group_mode = mode;
            track.config.filters.alignment_group_order = direction;
        }
        if track_ids.contains(&self.primary_alignment_track_id()) {
            self.activate_first_alignment_track()?;
        }
        self.invalidate_automation_alignment_layouts(Some(track_ids), false);
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
        Ok(())
    }

    fn automation_alignment_group_state(&self, track_ids: &[TrackId]) -> Vec<serde_json::Value> {
        self.alignment_tracks
            .iter()
            .filter(|track| track_ids.contains(&track.config.id))
            .map(|track| {
                serde_json::json!({
                    "track_id": track.config.id.get(),
                    "mode": track.config.filters.alignment_group_mode,
                    "direction": track.config.filters.alignment_group_order,
                })
            })
            .collect()
    }

    fn automation_alignment_sort_state(&self, track_ids: &[TrackId]) -> Vec<serde_json::Value> {
        self.alignment_tracks
            .iter()
            .filter(|track| track_ids.contains(&track.config.id))
            .map(|track| {
                serde_json::json!({
                    "track_id": track.config.id.get(),
                    "mode": track.config.filters.alignment_sort_mode,
                    "direction": track.config.filters.alignment_sort_order,
                })
            })
            .collect()
    }

    fn automation_alignment_filter_state(&self, track_ids: &[TrackId]) -> Vec<serde_json::Value> {
        self.alignment_tracks
            .iter()
            .filter(|track| track_ids.contains(&track.config.id))
            .map(|track| {
                serde_json::json!({
                    "track_id": track.config.id.get(),
                    "expression": &track.filter_editor.expression,
                })
            })
            .collect()
    }

    pub(crate) fn apply_automation_sort(
        &mut self,
        track_ids: &[TrackId],
        mode: AlignmentSortMode,
        direction: AlignmentOrderDirection,
        tag: Option<&str>,
    ) -> Result<()> {
        self.ensure_automation_alignment_tracks(track_ids)?;
        let mode = tag.map_or(Ok(mode), |tag| {
            parse_automation_sam_tag(tag).map(AlignmentSortMode::Tag)
        })?;
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.contains(&track.config.id))
        {
            track.config.filters.alignment_sort_mode = mode;
            track.config.filters.alignment_sort_order = direction;
        }
        if track_ids.contains(&self.primary_alignment_track_id()) {
            self.activate_first_alignment_track()?;
        }
        self.invalidate_automation_alignment_layouts(Some(track_ids), false);
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
        Ok(())
    }

    pub(crate) fn apply_automation_filter(
        &mut self,
        track_ids: &[TrackId],
        expression: &str,
    ) -> Result<()> {
        self.ensure_automation_alignment_tracks(track_ids)?;
        let predicates = expression
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<AuxPredicate>().map(|_| part.to_owned()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.contains(&track.config.id))
        {
            track
                .config
                .filters
                .auxiliary_predicates
                .clone_from(&predicates);
            let (editor, active) = filter_state_from_session(&track.config.filters)?;
            track.filter_editor = editor;
            track.active_filters = active;
        }
        if track_ids.contains(&self.primary_alignment_track_id()) {
            self.activate_first_alignment_track()?;
        }
        self.invalidate_automation_alignment_layouts(Some(track_ids), true);
        self.request_all_panel_queries();
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
        Ok(())
    }

    pub(crate) fn ensure_automation_alignment_tracks(&self, track_ids: &[TrackId]) -> Result<()> {
        for id in track_ids {
            anyhow::ensure!(
                self.alignment_tracks
                    .iter()
                    .any(|track| track.config.id == *id),
                "alignment track {} was not found",
                id.get()
            );
        }
        Ok(())
    }

    pub(crate) fn invalidate_automation_alignment_layouts(
        &mut self,
        track_ids: Option<&[TrackId]>,
        query_changed: bool,
    ) {
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.is_none_or(|track_ids| track_ids.contains(&track.config.id)))
        {
            for projection in track.projections.values_mut() {
                projection.alignment_rows = None;
                projection.cached_layout = None;
                if query_changed {
                    projection.clock.advance();
                    projection.pending_query = track.config.visible;
                }
            }
        }
        self.layout_cache.clear();
        if query_changed {
            self.automation_alignment_handles.clear();
            self.mark_retained_evidence_stale(
                "alignment query settings changed after this evidence was captured",
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_automation_selection(
        &mut self,
        selection: &AutomationSelection,
    ) -> Result<serde_json::Value> {
        match *selection {
            AutomationSelection::Panel { panel_id } => {
                anyhow::ensure!(
                    self.activate_view_panel(panel_id),
                    "panel {} was not found",
                    panel_id.get()
                );
                let revision = self.bump_workspace_revision();
                Ok(serde_json::json!({"panel_id": panel_id.get(), "workspace_revision": revision}))
            }
            AutomationSelection::Track { track_id } => {
                let key = self.automation_track_key(track_id)?;
                self.panels.active_mut().clear_selection();
                self.select_track_key(key);
                self.bump_workspace_revision();
                self.retain_active_selection(None);
                Ok(serde_json::json!({
                    "track_id": track_id.get(),
                    "workspace_revision": self.workspace_revision,
                    "selection": self.automation_selection_snapshot(),
                }))
            }
            AutomationSelection::Position {
                panel_id,
                track_id,
                position,
                granularity,
            } => {
                anyhow::ensure!(
                    self.activate_view_panel(panel_id),
                    "panel {} was not found",
                    panel_id.get()
                );
                let key = self.automation_track_key(track_id)?;
                anyhow::ensure!(
                    position < self.panels.active().contig_length,
                    "position {position} lies beyond contig {} length {}",
                    self.panels.active().contig_name,
                    self.panels.active().contig_length
                );
                if !self
                    .panels
                    .active_mut()
                    .viewport
                    .interval()
                    .contains(position)
                {
                    self.panels.active_mut().viewport = self
                        .panels
                        .active()
                        .viewport
                        .centered_on(position, self.panels.active().contig_length)?;
                    self.panels.active().generation_clock.advance();
                    self.request_alignment_query();
                    self.request_reference_query();
                    self.request_track_query();
                }
                let auxiliary_selection = if matches!(key, TrackFrameKey::Auxiliary(_)) {
                    self.panels
                        .active_mut()
                        .track_snapshots
                        .iter()
                        .find(|snapshot| snapshot.id == track_id)
                        .and_then(|snapshot| match &snapshot.data {
                            TrackSnapshotData::Variants(query) => query
                                .records
                                .iter()
                                .enumerate()
                                .filter(|(_, record)| {
                                    record.start <= position
                                        && position < record.end.max(record.start.saturating_add(1))
                                })
                                .min_by_key(|(index, record)| {
                                    (record.end.saturating_sub(record.start), *index)
                                })
                                .map(|(index, _)| (Some(index), None)),
                            TrackSnapshotData::Quantitative(query) => {
                                quantitative_record_index_at_position(&query.records, position)
                                    .map(|index| (None, Some(index)))
                            }
                            TrackSnapshotData::Features(_)
                            | TrackSnapshotData::CopyNumber(_)
                            | TrackSnapshotData::Mutations(_)
                            | TrackSnapshotData::Interactions(_)
                            | TrackSnapshotData::Gwas(_) => None,
                        })
                } else {
                    None
                };
                self.panels.active_mut().clear_selection();
                self.panels.active_mut().selected_track = Some(key);
                if matches!(key, TrackFrameKey::Alignment(_)) {
                    self.panels.active_mut().selected_alignment_track = Some(track_id);
                    self.panels.active_mut().selected_alignment_tracks = vec![track_id];
                    self.panels.active_mut().selected_pileup_position = Some(position);
                } else if let Some((variant, quantitative)) = auxiliary_selection {
                    self.panels.active_mut().selected_variant =
                        variant.map(|record_index| VariantSelection {
                            track_id,
                            record_index,
                        });
                    self.panels.active_mut().selected_quantitative =
                        quantitative.map(|record_index| QuantitativeSelection {
                            track_id,
                            record_index,
                            position,
                        });
                }
                self.rebuild_genomic_display();
                self.display_dirty = true;
                self.bump_workspace_revision();
                self.retain_active_selection(Some(granularity));
                Ok(serde_json::json!({
                    "panel_id": panel_id.get(),
                    "track_id": track_id.get(),
                    "position": position,
                    "workspace_revision": self.workspace_revision,
                    "selection": self.automation_selection_snapshot(),
                }))
            }
        }
    }

    pub(crate) fn automation_exact_base_available(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        position: u64,
    ) -> bool {
        self.alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .and_then(|track| track.projection(panel_id))
            .and_then(|projection| projection.current_query.as_deref())
            .and_then(|query| {
                let (index, _) = query.pileup.bin_at(query.interval, position)?;
                query.pileup.bin_interval(query.interval, index)
            })
            .is_some_and(|interval| interval.start() == position && interval.end() == position + 1)
    }

    pub(crate) fn automation_track_key(&self, track_id: TrackId) -> Result<TrackFrameKey> {
        if self
            .alignment_tracks
            .iter()
            .any(|track| track.config.id == track_id)
        {
            return Ok(TrackFrameKey::Alignment(track_id));
        }
        if self
            .tracks
            .tracks()
            .iter()
            .any(|track| track.id == track_id)
        {
            return Ok(TrackFrameKey::Auxiliary(track_id));
        }
        anyhow::bail!("track {} was not found", track_id.get())
    }

    pub(crate) fn start_automation_snapshot(
        &mut self,
        command_index: usize,
        path: PathBuf,
        scope: AutomationSnapshotScope,
        format: AutomationSnapshotFormat,
    ) -> Result<()> {
        let (display, width, height, scope_label) = match scope {
            AutomationSnapshotScope::VisibleWorkspace => {
                let width = logical_surface_dimension(
                    self.surface_config.width,
                    self.window.scale_factor(),
                );
                let height = logical_surface_dimension(
                    self.surface_config.height,
                    self.window.scale_factor(),
                );
                (
                    self.composite_display_list.clone(),
                    width,
                    height,
                    "visible workspace".to_owned(),
                )
            }
            AutomationSnapshotScope::ActivePanel => {
                let bounds = self.active_panel_rect();
                (
                    cropped_display_list(&self.panels.active_mut().display_list, bounds),
                    bounds.width().ceil().max(1.0) as u32,
                    bounds.height().ceil().max(1.0) as u32,
                    "active panel".to_owned(),
                )
            }
            AutomationSnapshotScope::FullActivePanel => self.full_active_panel_export_display(),
        };
        let mut metadata = automation_snapshot_metadata(
            &self.assembly_name,
            &self.panels,
            &self.alignment_tracks,
            &self.tracks,
            &scope_label,
        );
        let _ = write!(
            metadata,
            "; application instance {}; workspace revision {}",
            self.application_instance_id, self.workspace_revision
        );
        if let Some(selection) = self.automation_selection_snapshot() {
            let _ = write!(
                metadata,
                "; selection {}; evidence revision {}",
                selection.selection_id, selection.evidence_revision
            );
        }
        let truncated = scope_label.contains("truncated");
        let completion_data = serde_json::json!({
            "width": width,
            "height": height,
            "scope": scope,
            "scope_description": scope_label,
            "truncated": truncated,
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "selection_id": self.automation_selection_snapshot().map(|selection| selection.selection_id),
            "evidence_revision": self.automation_selection_snapshot().map(|selection| selection.evidence_revision),
        });
        self.start_automation_export_job_with_data(
            command_index,
            path,
            completion_data,
            move |path| match format {
                AutomationSnapshotFormat::Png => export_png(&display, width, height, path)
                    .map_err(|error| std::io::Error::other(error.to_string())),
                AutomationSnapshotFormat::Svg => std::fs::write(
                    path,
                    display_list_svg_export(&display, width, height, &metadata),
                ),
            },
        )
    }

    pub(crate) fn full_active_panel_export_display(&mut self) -> (DisplayList, u32, u32, String) {
        let original_geometry = self.canvas_geometry;
        let original_scroll = self.panels.active().vertical_scroll_points;
        let content_height = (self.panels.active().content_bottom_points
            - self.canvas_geometry.viewport.top())
        .ceil()
        .max(1.0);
        let export_height = content_height.min(MAX_AUTOMATION_FULL_PANEL_HEIGHT as f32) as u32;
        let truncated = content_height > MAX_AUTOMATION_FULL_PANEL_HEIGHT as f32;
        let expanded_viewport = egui::Rect::from_min_max(
            original_geometry.viewport.min,
            egui::pos2(
                original_geometry.viewport.right(),
                original_geometry.viewport.top() + export_height as f32,
            ),
        );
        self.canvas_geometry = CanvasGeometry::from_viewport(expanded_viewport);
        self.panels.active_mut().vertical_scroll_points = 0.0;
        self.rebuild_genomic_display();
        let bounds = self.active_panel_rect();
        let display = cropped_display_list(&self.panels.active().display_list, bounds);
        let width = bounds.width().ceil().max(1.0) as u32;

        self.canvas_geometry = original_geometry;
        self.panels.active_mut().vertical_scroll_points = original_scroll;
        self.rebuild_genomic_display();
        self.composite_display_list = composite_panel_display_lists(&self.panels);
        self.display_dirty = true;

        let scope = if truncated {
            format!(
                "full active panel; truncated from {content_height:.0} to {MAX_AUTOMATION_FULL_PANEL_HEIGHT} logical pixels"
            )
        } else {
            "full active panel".to_owned()
        };
        (display, width, export_height, scope)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn start_automation_export(
        &mut self,
        command_index: usize,
        path: PathBuf,
        kind: AutomationExportKind,
    ) -> Result<()> {
        let assembly = self.assembly_name.clone();
        let contig = self.panels.active().contig_name.clone();
        let interval = self.panels.active().viewport.interval();
        match kind {
            AutomationExportKind::Regions => {
                let regions = self.regions_of_interest.clone();
                self.start_automation_export_job(command_index, path, move |path| {
                    std::fs::write(path, region_bed_export(&regions))
                })
            }
            AutomationExportKind::TrackNames => {
                let contents = automation_track_names_export(
                    &self.assembly_name,
                    &self.panels,
                    &self.alignment_tracks,
                    &self.tracks,
                );
                self.start_automation_export_job(command_index, path, move |path| {
                    std::fs::write(path, contents)
                })
            }
            AutomationExportKind::Alignments
            | AutomationExportKind::Pileup
            | AutomationExportKind::Modifications
            | AutomationExportKind::Junctions => {
                let tracks = self
                    .alignment_tracks
                    .iter()
                    .filter(|track| track.config.visible)
                    .filter_map(|track| {
                        self.alignment_query_for_track(track.config.id)
                            .map(|query| AutomationAlignmentExportTrack {
                                id: track.config.id,
                                name: track.config.name.clone(),
                                query: Arc::clone(query),
                            })
                    })
                    .collect::<Vec<_>>();
                anyhow::ensure!(
                    !tracks.is_empty(),
                    "no visible settled alignment data is available to export"
                );
                self.start_automation_export_job(command_index, path, move |path| {
                    let contents = match kind {
                        AutomationExportKind::Alignments => {
                            automation_alignments_export(&assembly, &contig, interval, &tracks)
                        }
                        AutomationExportKind::Pileup => {
                            automation_pileup_export(&assembly, &contig, interval, &tracks)
                        }
                        AutomationExportKind::Modifications => {
                            automation_modifications_export(&assembly, &contig, interval, &tracks)
                        }
                        AutomationExportKind::Junctions => {
                            automation_junctions_export(&assembly, &contig, interval, &tracks)
                        }
                        _ => unreachable!(),
                    };
                    std::fs::write(path, contents)
                })
            }
            AutomationExportKind::Variants | AutomationExportKind::Quantitative => {
                let visible_ids = self
                    .tracks
                    .tracks()
                    .iter()
                    .filter(|track| track.visible)
                    .map(|track| track.id)
                    .collect::<BTreeSet<_>>();
                let snapshots = self
                    .panels
                    .active()
                    .track_snapshots
                    .iter()
                    .filter(|snapshot| visible_ids.contains(&snapshot.id))
                    .cloned()
                    .collect::<Vec<_>>();
                let has_compatible = snapshots.iter().any(|snapshot| match kind {
                    AutomationExportKind::Variants => {
                        matches!(snapshot.data, TrackSnapshotData::Variants(_))
                    }
                    AutomationExportKind::Quantitative => {
                        matches!(snapshot.data, TrackSnapshotData::Quantitative(_))
                    }
                    _ => false,
                });
                anyhow::ensure!(
                    has_compatible,
                    "no visible settled data is available to export"
                );
                self.start_automation_export_job(command_index, path, move |path| {
                    let contents = if kind == AutomationExportKind::Variants {
                        automation_variants_export(&assembly, &contig, interval, &snapshots)
                    } else {
                        automation_quantitative_export(&assembly, &contig, interval, &snapshots)
                    };
                    std::fs::write(path, contents)
                })
            }
            AutomationExportKind::SelectedEvidence => {
                if let Some(retained) = self.panels.active().retained_evidence.clone()
                    && !retained.export.is_empty()
                {
                    anyhow::ensure!(
                        !retained.selection.stale,
                        "selected evidence is stale; refresh it explicitly before export"
                    );
                    let mut digest = String::with_capacity(64);
                    for byte in Sha256::digest(retained.export.as_bytes()) {
                        let _ = write!(digest, "{byte:02x}");
                    }
                    let data = serde_json::json!({
                        "selection_id": retained.selection.selection_id,
                        "evidence_revision": retained.selection.evidence_revision,
                        "workspace_revision": self.workspace_revision,
                        "bytes": retained.export.len(),
                        "sha256": digest,
                    });
                    return self.start_automation_export_job_with_data(
                        command_index,
                        path,
                        data,
                        move |path| std::fs::write(path, retained.export),
                    );
                }
                let contents = if let Some(inspector) = self.variant_inspector_snapshot() {
                    let evidence = self.variant_alignment_evidence(&inspector);
                    format!(
                        "{}\n{}",
                        variant_genotype_export_tsv(&inspector),
                        variant_evidence_export_tsv(&inspector, &evidence)
                    )
                } else if let Some(inspector) = self.gwas_inspector_snapshot() {
                    automation_inspector_export(&InspectorSnapshot {
                        fields: gwas_inspector_fields(&inspector),
                        auxiliary: Vec::new(),
                        event_sequence: None,
                        cigar: None,
                        read_sequence: None,
                    })
                } else {
                    let inspector = self
                        .inspector_snapshot()
                        .or_else(|| self.pileup_inspector_snapshot())
                        .or_else(|| self.modification_inspector_snapshot())
                        .or_else(|| self.junction_inspector_snapshot())
                        .context("no selected evidence is available to export")?;
                    automation_inspector_export(&inspector)
                };
                self.start_automation_export_job(command_index, path, move |path| {
                    std::fs::write(path, contents)
                })
            }
        }
    }

    pub(crate) fn start_automation_export_job(
        &mut self,
        command_index: usize,
        path: PathBuf,
        job: impl FnOnce(PathBuf) -> std::io::Result<()> + Send + 'static,
    ) -> Result<()> {
        self.start_automation_export_job_with_data(command_index, path, serde_json::json!({}), job)
    }

    pub(crate) fn start_automation_export_job_with_data(
        &mut self,
        command_index: usize,
        path: PathBuf,
        completion_data: serde_json::Value,
        job: impl FnOnce(PathBuf) -> std::io::Result<()> + Send + 'static,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.export_tasks.iter().any(|task| task.path == path),
            "an export to {} is already in progress",
            path.display()
        );
        let display_path = path.display().to_string();
        let worker = std::thread::Builder::new()
            .name("rgv-automation-export".to_owned())
            .spawn({
                let path = path.clone();
                move || {
                    create_output_parent(&path)?;
                    job(path)
                }
            })?;
        self.export_tasks.push(ExportTask {
            path,
            automation_command: Some(command_index),
            automation_data: Some(completion_data),
            worker,
        });
        self.data_status = format!("Automation is exporting {display_path}…");
        Ok(())
    }

    pub(crate) fn start_automation_session_save(
        &mut self,
        command_index: usize,
        path: PathBuf,
    ) -> Result<()> {
        anyhow::ensure!(
            !self.session_save_tasks.iter().any(|task| task.path == path),
            "a save to {} is already in progress",
            path.display()
        );
        let document = self.session_document();
        let display_path = path.display().to_string();
        let worker = std::thread::Builder::new()
            .name("rgv-automation-session-save".to_owned())
            .spawn({
                let path = path.clone();
                move || document.write(path).map_err(|error| error.to_string())
            })?;
        self.session_save_tasks.push(SessionSaveTask {
            path,
            kind: SessionSaveKind::Manual,
            automation_command: Some(command_index),
            worker,
        });
        self.data_status = format!("Automation is saving {display_path}…");
        Ok(())
    }
}
