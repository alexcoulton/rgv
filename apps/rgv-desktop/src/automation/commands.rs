use super::super::*;
use crate::display::alignment;

const MAXIMUM_AUTOMATION_SUPPLEMENTARY_SEGMENTS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutomationReadPresentationFields {
    pub(crate) state: AutomationReadPresentationState,
    pub(crate) requested_limit: usize,
    pub(crate) effective_limit: usize,
    pub(crate) eligible_records: usize,
    pub(crate) complete_records: usize,
    pub(crate) refusal_eligible_records: usize,
    pub(crate) refusal_scope: Option<String>,
}

pub(crate) fn automation_read_presentation_fields(
    query: Option<&AlignmentQuery>,
    layout_query: Option<&AlignmentQuery>,
    stacking_mode: ReadStackingMode,
    checking: bool,
    failed: bool,
    requested_limit: usize,
) -> AutomationReadPresentationFields {
    let refusal = query.and_then(|query| {
        alignment::individual_read_refusal(query, layout_query.unwrap_or(query), stacking_mode)
    });
    let state = if failed {
        AutomationReadPresentationState::Failed
    } else if checking {
        AutomationReadPresentationState::Checking
    } else if refusal.is_some() {
        AutomationReadPresentationState::TooDense
    } else if query.is_some() {
        AutomationReadPresentationState::Complete
    } else {
        AutomationReadPresentationState::Unavailable
    };
    AutomationReadPresentationFields {
        state,
        requested_limit,
        effective_limit: query.map_or(requested_limit, |query| query.stats.complete_read_limit),
        eligible_records: query.map_or(0, |query| query.stats.records_eligible),
        complete_records: query.map_or(0, |query| {
            if refusal.is_none() && query.individual_reads_complete() {
                query.stats.records_emitted
            } else {
                0
            }
        }),
        refusal_eligible_records: refusal.map_or(0, |refusal| refusal.eligible_records),
        refusal_scope: refusal.map(|refusal| match refusal.scope {
            IndividualReadRefusalScope::Foreground => "foreground".to_owned(),
            IndividualReadRefusalScope::LayoutHorizon => "layout_horizon".to_owned(),
        }),
    }
}

fn automation_locus(contig: &str, interval: GenomicInterval) -> String {
    format!(
        "{}:{}-{}",
        contig,
        interval.start().saturating_add(1),
        interval.end()
    )
}

fn automation_complete_alignment_query(
    track: &AlignmentTrackController,
    panel_id: PanelId,
    presentation_committed: bool,
) -> Result<Arc<AlignmentQuery>> {
    anyhow::ensure!(
        presentation_committed,
        "the target viewport is still being checked"
    );
    let projection = track
        .projection(panel_id)
        .context("the alignment query is unavailable for this panel")?;
    anyhow::ensure!(
        projection.query_is_current(),
        "the alignment query has not settled for the current viewport"
    );
    let query = projection
        .current_query
        .as_ref()
        .cloned()
        .context("the alignment query has not settled")?;
    let layout_query = projection.layout_query.as_deref().unwrap_or(&query);
    if let Some(refusal) = alignment::individual_read_refusal(
        &query,
        layout_query,
        track.config.filters.read_stacking_mode,
    ) {
        anyhow::bail!(alignment::read_refusal_detail(refusal));
    }
    Ok(query)
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
    viewport_contig: &str,
) -> AutomationSelectionSnapshot {
    let mut snapshot = retained.clone();
    snapshot.visible_in_active_view = snapshot.contig == viewport_contig
        && snapshot
            .evidence_start
            .zip(snapshot.evidence_end)
            .and_then(|(start, end)| GenomicInterval::new(viewport.contig(), start, end).ok())
            .is_some_and(|evidence| viewport.contains_interval(evidence));
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
        !self.display_dirty && self.next_egui_repaint.is_none() && self.automation_data_is_settled()
    }

    /// Whether the observable workspace data has settled. Receiving a local-control
    /// request dirties the status bar before its first command runs, so observation
    /// snapshots must not treat that control-only repaint as unsettled genomic data.
    fn automation_data_is_settled(&self) -> bool {
        if !self.export_tasks.is_empty()
            || self.pending_native_window_capture.is_some()
            || self.pending_resolution_query_refresh.is_some()
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
            if panel.navigation_presentation.is_unsettled()
                || panel.navigation_query_schedule.pending.is_some()
            {
                return false;
            }
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
                && panel.viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
                && (panel.reference_slice.as_ref().is_none_or(|slice| {
                    !slice.interval.contains_interval(panel.viewport.interval())
                }) || panel.reference_settled_generation
                    != Some(panel.generation_clock.current().get()))
            {
                return false;
            }
            for track in &self.alignment_tracks {
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
                                && projection.unavailable.is_none()
                                && !projection.index_only_overview)
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
            AutomationCommand::DescribeInteractionTrace => {
                let trace = self.interaction_trace.snapshot();
                let message = if trace.is_some() {
                    "Described the most recent vertical scrollbar interaction"
                } else {
                    "No vertical scrollbar interaction has been recorded"
                };
                match serde_json::to_value(trace) {
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
                session.assembly_identity = self.assembly_identity.clone();
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
                Err(error)
                    if error
                        .to_string()
                        .starts_with("modified-base evidence is unavailable") =>
                {
                    automation_failure(AutomationErrorCode::EffectiveStateUnavailable, error)
                }
                Err(error) if error.to_string().starts_with("complete reads unavailable") => {
                    automation_failure(AutomationErrorCode::TooDense, error)
                }
                Err(error) => automation_failure(AutomationErrorCode::ResourceNotFound, error),
            },
            AutomationCommand::SelectAlignment {
                handle,
                reveal,
                expected_workspace_revision,
            } => {
                if expected_workspace_revision
                    .is_some_and(|expected| expected != self.workspace_revision)
                {
                    return automation_failure(
                        AutomationErrorCode::WorkspaceRevisionConflict,
                        anyhow::anyhow!(
                            "workspace revision changed: expected {}, observed {}",
                            expected_workspace_revision.unwrap_or_default(),
                            self.workspace_revision
                        ),
                    );
                }
                match self.select_automation_alignment(&handle, reveal) {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Selected an opaque alignment handle".to_owned(),
                        data,
                    },
                    Err(error) if error.to_string().starts_with("complete reads unavailable") => {
                        automation_failure(AutomationErrorCode::TooDense, error)
                    }
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
                expected_workspace_revision,
            } => {
                if expected_workspace_revision
                    .is_some_and(|expected| expected != self.workspace_revision)
                {
                    return automation_failure(
                        AutomationErrorCode::WorkspaceRevisionConflict,
                        anyhow::anyhow!(
                            "workspace revision changed: expected {}, observed {}",
                            expected_workspace_revision.unwrap_or_default(),
                            self.workspace_revision
                        ),
                    );
                }
                match self.navigate_automation_related_alignment(
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
                }
            }
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
                    Err(error) if error.to_string().starts_with("complete reads unavailable") => {
                        automation_failure(AutomationErrorCode::TooDense, error)
                    }
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
        let shared_annotation_scroll = self.panels.annotation_scroll_points;
        let maximum_shared_annotation_scroll = self.panels.maximum_annotation_scroll_points();
        let mut panels: Vec<_> = self
            .panels
            .iter()
            .map(|panel| {
                let interval = panel.viewport.interval();
                let maximum_scroll = (panel.content_bottom_points
                    - self.canvas_geometry().viewport.bottom())
                .max(0.0);
                let maximum_annotation_scroll = maximum_shared_annotation_scroll;
                let filters = self.primary_alignment_filters();
                let alignment_projection = self
                    .alignment_tracks
                    .first()
                    .and_then(|track| track.projection(panel.id));
                let query = alignment_projection
                    .filter(|_| panel.navigation_presentation.permits_staged_display())
                    .filter(|projection| projection.query_is_current())
                    .and_then(|projection| projection.current_query.as_deref());
                let layout_query = alignment_projection
                    .filter(|_| panel.navigation_presentation.permits_staged_display())
                    .filter(|projection| projection.query_is_current())
                    .and_then(|projection| projection.layout_query.as_deref());
                let read_refusal = self
                    .alignment_tracks
                    .first()
                    .and_then(|track| {
                        query.map(|query| {
                            alignment::individual_read_refusal(
                                query,
                                layout_query.unwrap_or(query),
                                track.config.filters.read_stacking_mode,
                            )
                        })
                    })
                    .flatten();
                let index_only_overview =
                    alignment_projection.is_some_and(|projection| projection.index_only_overview);
                let layout_stats = self
                    .alignment_tracks
                    .first()
                    .and_then(|track| track.projection(panel.id))
                    .and_then(|projection| projection.cached_layout.as_ref())
                    .map(|(_, layout)| layout.stats);
                let read_content_visible = self
                    .track_presentation
                    .effective_track_visibility(
                        ui::TrackPane::Reads,
                        self.primary_alignment_track_id(),
                    )
                    .unwrap_or(false);
                let reads_rendered = read_content_visible
                    && panel.viewport.detail_level() != DetailLevel::Overview
                    && query.is_some()
                    && read_refusal.is_none();
                let render_lanes = [
                    ("reference", filters.show_reference),
                    ("coverage", filters.show_coverage),
                    ("reads", read_content_visible),
                    ("modifications", filters.show_modified_bases),
                ]
                .into_iter()
                .map(|(lane, configured)| {
                    let rendered = configured
                        && query.is_some()
                        && (lane != "reads" && lane != "modifications" || reads_rendered);
                    let omission_reason = (!rendered).then_some(if !configured {
                        AutomationRenderOmissionReason::Disabled
                    } else if (lane == "reads" || lane == "modifications") && read_refusal.is_some()
                    {
                        AutomationRenderOmissionReason::TooDense
                    } else if index_only_overview {
                        AutomationRenderOmissionReason::OverviewDetailThreshold
                    } else if query.is_none() {
                        if panel.pending_queries.alignment
                            || alignment_projection
                                .is_some_and(AlignmentPanelProjection::query_is_checking)
                        {
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
                        rendered_items: match lane {
                            "reads" => layout_stats.map_or(0, |stats| stats.rendered_alignments),
                            "coverage" => query.map_or(0, |query| query.stats.records_eligible),
                            "modifications" => query.map_or(0, |query| query.stats.records_emitted),
                            _ => 0,
                        },
                        sampled_items: 0,
                        truncated: query.is_some_and(|query| query.stats.truncated),
                        filtered_unmapped_items: query
                            .map_or(0, |query| query.stats.records_filtered_unmapped),
                        filtered_secondary_items: query
                            .map_or(0, |query| query.stats.records_filtered_secondary),
                        filtered_supplementary_items: query
                            .map_or(0, |query| query.stats.records_filtered_supplementary),
                        filtered_user_items: query
                            .map_or(0, |query| query.stats.records_filtered_user),
                        filtered_duplicate_items: query
                            .map_or(0, |query| query.stats.records_filtered_duplicate),
                        filtered_qc_failed_items: query
                            .map_or(0, |query| query.stats.records_filtered_qc_failed),
                        regional_insertions_rendered: layout_stats
                            .map_or(0, |stats| stats.regional_insertions_rendered),
                        regional_insertions_collapsed: layout_stats
                            .map_or(0, |stats| stats.regional_insertions_collapsed),
                        regional_insertions_omitted: layout_stats
                            .map_or(0, |stats| stats.regional_insertions_omitted),
                        raw_maximum_coverage: layout_stats
                            .map_or(0, |stats| stats.maximum_coverage),
                        coverage_exact_complete: query
                            .is_some_and(|query| query.pileup.provenance.exact_depth_complete),
                        depth_segments_omitted: query
                            .map_or(0, |query| query.pileup.provenance.depth_segments_omitted),
                        packed_row_count: layout_stats.map_or(0, |stats| stats.packed_row_count),
                        visible_occupied_rows: layout_stats
                            .map_or(0, |stats| stats.visible_occupied_rows),
                        visible_row_span: layout_stats.map_or(0, |stats| stats.visible_row_span),
                        natural_stack_height: layout_stats
                            .map_or(0, |stats| stats.natural_stack_height),
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
                    pixel_width: panel.viewport.pixel_width(),
                    bases_per_pixel: panel.viewport.bases_per_pixel(),
                    vertical_scroll_points: panel.vertical_scroll_points,
                    maximum_vertical_scroll_points: maximum_scroll,
                    annotation_scroll_points: shared_annotation_scroll,
                    maximum_annotation_scroll_points: maximum_annotation_scroll,
                    read_stack_scroll_points: panel.read_stack_scroll_points,
                    maximum_read_stack_scroll_points: (panel.pane_layout.reads.content_bottom
                        - panel.pane_layout.reads.pane_rect.bottom())
                    .max(0.0),
                    render_generation: panel.generation_clock.current().get(),
                    settled: !panel.pending_queries.alignment
                        && !panel.pending_queries.reference
                        && !panel.pending_queries.tracks
                        && !panel.navigation_presentation.is_unsettled()
                        && panel.navigation_query_schedule.pending.is_none()
                        && self
                            .alignment_tracks
                            .iter()
                            .all(|track| match track.lifecycle {
                                AlignmentTrackLifecycle::Opening => false,
                                AlignmentTrackLifecycle::Failed(_) => true,
                                AlignmentTrackLifecycle::Ready => track
                                    .projection(panel.id)
                                    .is_some_and(AlignmentPanelProjection::query_is_current),
                            }),
                    navigation_transaction_active: panel.navigation_presentation.is_unsettled(),
                    navigation_preview_active: panel
                        .navigation_presentation
                        .shows_retained_preview(),
                    preview_source_locus: panel
                        .navigation_presentation
                        .source
                        .as_ref()
                        .map(|source| automation_locus(&panel.contig_name, source.interval)),
                    preview_target_locus: panel.navigation_presentation.is_unsettled().then(|| {
                        automation_locus(
                            &panel.contig_name,
                            panel.navigation_presentation.target_interval,
                        )
                    }),
                    preview_source_generation: panel
                        .navigation_presentation
                        .source
                        .as_ref()
                        .map(|source| source.generation),
                    preview_target_generation: panel
                        .navigation_presentation
                        .is_unsettled()
                        .then_some(panel.navigation_presentation.target_generation),
                    preview_age_ms: panel.navigation_presentation.is_unsettled().then(|| {
                        u64::try_from(
                            panel
                                .navigation_presentation
                                .age(Instant::now())
                                .as_millis(),
                        )
                        .unwrap_or(u64::MAX)
                    }),
                    preview_kind: panel
                        .navigation_presentation
                        .preview_kind
                        .map(|kind| kind.label().to_owned()),
                    preview_invalidation_reason: panel
                        .navigation_presentation
                        .invalidation_reason
                        .clone(),
                    render_lanes,
                    read_panel_states: self
                        .alignment_tracks
                        .iter()
                        .map(|track| {
                            let track_id = track.config.id;
                            let state = panel.read_panel_scroll_state(track_id);
                            let clip_bottom = panel
                                .scroll_display_cache
                                .as_ref()
                                .and_then(|cache| {
                                    cache
                                        .read_panels
                                        .iter()
                                        .find(|read_panel| read_panel.track_id == track_id)
                                })
                                .map_or(panel.pane_layout.reads.pane_rect.bottom(), |read_panel| {
                                    read_panel.clip_bottom
                                });
                            AutomationReadPanelStateSnapshot {
                                track_id,
                                scroll_points: state.scroll_points,
                                maximum_scroll_points: (state.content_bottom_points - clip_bottom)
                                    .max(0.0),
                                content_origin_points: state.content_origin_points,
                                latent_scroll_fraction: state.latent_scroll_fraction,
                                content_bottom_points: state.content_bottom_points,
                                density: state.density,
                                effective_density: state
                                    .density
                                    .unwrap_or(track.config.filters.alignment_density),
                            }
                        })
                        .collect(),
                    selected_presentation_nodes: panel.selected_presentation_nodes.clone(),
                    presentation_selection_anchor: panel.presentation_selection_anchor,
                }
            })
            .collect();
        let active_presentation_committed = self
            .panels
            .active()
            .navigation_presentation
            .permits_staged_display();
        let mut tracks = self
            .alignment_tracks
            .iter()
            .map(|track| {
                let projection = track.projection(self.panels.active().id);
                let settled_query = projection
                    .filter(|_| active_presentation_committed)
                    .filter(|projection| projection.query_is_current())
                    .and_then(|projection| projection.current_query.as_deref());
                let settled_layout_query = projection
                    .filter(|_| active_presentation_committed)
                    .filter(|projection| projection.query_is_current())
                    .and_then(|projection| projection.layout_query.as_deref());
                let reconciliation = projection
                    .and_then(|projection| projection.alignment_rows.as_ref())
                    .map_or_else(AlignmentRowReconciliationStats::default, |cached| {
                        cached.reconciliation
                    });
                let (lifecycle, failure) = match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => (AutomationTrackLifecycle::Opening, None),
                    AlignmentTrackLifecycle::Ready => (AutomationTrackLifecycle::Ready, None),
                    AlignmentTrackLifecycle::Failed(_) => (
                        AutomationTrackLifecycle::Failed,
                        Some("alignment track failed to open".to_owned()),
                    ),
                };
                let read_presentation = automation_read_presentation_fields(
                    settled_query,
                    settled_layout_query,
                    track.config.filters.read_stacking_mode,
                    lifecycle == AutomationTrackLifecycle::Opening
                        || projection.is_some_and(AlignmentPanelProjection::query_is_checking),
                    lifecycle == AutomationTrackLifecycle::Failed,
                    track.config.maximum_alignments,
                );
                let annotation = self
                    .track_presentation
                    .pane_track_state(ui::TrackPane::Annotation, track.config.id);
                let annotation_visible = annotation.is_none_or(|node| node.content_visible);
                let read_visible = self
                    .track_presentation
                    .pane_track_state(ui::TrackPane::Reads, track.config.id)
                    .is_none_or(|node| node.content_visible);
                AutomationTrackSnapshot {
                    id: track.config.id,
                    name: track.config.name.clone(),
                    kind: TrackKind::Alignment,
                    visible: annotation_visible,
                    collapsed: !annotation_visible,
                    height: annotation
                        .and_then(|node| node.preferred_height)
                        .unwrap_or(track.config.height),
                    source_available: lifecycle != AutomationTrackLifecycle::Failed,
                    lifecycle,
                    failure,
                    alignment: Some(AutomationAlignmentTrackSnapshot {
                        group_mode: track.config.filters.alignment_group_mode,
                        group_order: track.config.filters.alignment_group_order,
                        sort_mode: track.config.filters.alignment_sort_mode,
                        sort_order: track.config.filters.alignment_sort_order,
                        sort_anchor: track.config.filters.alignment_sort_anchor,
                        filter_expression: track.filter_editor.expression.clone(),
                        view_as_pairs: track.config.filters.view_as_pairs,
                        read_stacking_mode: track.config.filters.read_stacking_mode,
                        read_presentation_state: read_presentation.state,
                        requested_complete_read_limit: read_presentation.requested_limit,
                        effective_complete_read_limit: read_presentation.effective_limit,
                        eligible_read_records: read_presentation.eligible_records,
                        complete_read_records: read_presentation.complete_records,
                        refusal_eligible_read_records: read_presentation.refusal_eligible_records,
                        read_refusal_scope: read_presentation.refusal_scope,
                        row_layout_revision: reconciliation.revision,
                        row_layout_architecture_version: reconciliation.architecture_version,
                        row_layout_reconciliation_micros: reconciliation.reconciliation_micros,
                        row_layout_incumbent_projection_micros: reconciliation
                            .incumbent_projection_micros,
                        row_layout_history_index_micros: reconciliation.history_index_micros,
                        row_layout_allocation_micros: reconciliation.row_allocation_micros,
                        row_layout_history_commit_micros: reconciliation.history_commit_micros,
                        row_layout_canonical_metrics_micros: reconciliation
                            .canonical_metrics_micros,
                        row_layout_compatibility_probes: reconciliation.compatibility_probes,
                        row_layout_rows_reused: reconciliation.rows_reused,
                        row_layout_rows_appended: reconciliation.rows_appended,
                        row_layout_retained: reconciliation.retained,
                        row_layout_restored: reconciliation.restored,
                        row_layout_added: reconciliation.added,
                        row_layout_moved: reconciliation.moved,
                        row_layout_reservations: reconciliation.reservations,
                        row_layout_history_bytes: reconciliation.history_bytes,
                        row_layout_islands: reconciliation.islands,
                        row_layout_evictions: reconciliation.evictions,
                        row_layout_transition_reason: reconciliation.transition_reason,
                        row_layout_policy_fingerprint: reconciliation.policy_fingerprint,
                        row_layout_projection_fingerprint: reconciliation.projection_fingerprint,
                        row_layout_stable_rows: reconciliation.stable_rows,
                        row_layout_canonical_rows: reconciliation.canonical_rows,
                        row_layout_row_inflation: reconciliation.row_inflation,
                        row_layout_canonical_displacement: reconciliation.canonical_displacement,
                        row_layout_top_first_violations: reconciliation.top_first_violations,
                        row_layout_query_occupied_row_span: reconciliation.query_occupied_row_span,
                        row_layout_query_empty_rows: reconciliation.query_empty_rows,
                        row_layout_query_empty_ratio_ppm: reconciliation.query_empty_ratio_ppm,
                        row_layout_horizon_start: reconciliation.horizon_start,
                        row_layout_horizon_end: reconciliation.horizon_end,
                        row_layout_horizon_generation: reconciliation.horizon_generation,
                        row_layout_horizon_extensions: reconciliation.horizon_extensions,
                        row_layout_horizon_records: reconciliation.horizon_records,
                        row_layout_horizon_bytes: reconciliation.horizon_bytes,
                        row_layout_horizon_truncated: reconciliation.horizon_truncated,
                        row_layout_horizon_cache_hit: projection
                            .is_some_and(|projection| projection.layout_query_cache_hit),
                        stable_architecture_rebuild_pending: projection
                            .is_some_and(|projection| projection.pending_row_repack),
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
                        coverage_scale_transform: track.config.filters.coverage_scale_transform,
                        show_reads: read_visible,
                        include_secondary: track.config.filters.include_secondary,
                        include_supplementary: track.config.filters.include_supplementary,
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
            let annotation = self
                .track_presentation
                .pane_track_state(ui::TrackPane::Annotation, track.id);
            let annotation_visible = annotation.is_none_or(|node| node.content_visible);
            AutomationTrackSnapshot {
                id: track.id,
                name: track.name.clone(),
                kind: track.kind,
                visible: annotation_visible,
                collapsed: !annotation_visible,
                height: annotation
                    .and_then(|node| node.preferred_height)
                    .unwrap_or(track.height),
                source_available: lifecycle != AutomationTrackLifecycle::Failed,
                lifecycle,
                failure,
                alignment: None,
            }
        }));
        let source_available = |track_id: TrackId| {
            self.alignment_tracks
                .iter()
                .find(|track| track.config.id == track_id)
                .is_some_and(|track| !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_)))
                || self.tracks.tracks().iter().any(|track| {
                    track.id == track_id
                        && !matches!(
                            self.automation_auxiliary_open_state.get(&track_id),
                            Some(AutomationAuxiliaryOpenState::Failed(_))
                        )
                })
        };
        let parent_composite = |track_id: TrackId| {
            self.tracks
                .tracks()
                .iter()
                .find(|track| track.id == track_id)
                .and_then(|track| track.quantitative.overlay_group)
                .filter(|id| {
                    self.track_presentation.composites.iter().any(|composite| {
                        composite.channel == ui::TrackPane::Annotation && composite.id == *id
                    })
                })
        };
        let composite_visible = |track_id: TrackId| {
            parent_composite(track_id).is_none_or(|id| {
                self.track_presentation
                    .composite_content_visible(ui::TrackPane::Annotation, id)
            })
        };
        let mut presentation_nodes = Vec::new();
        for channel in [ui::TrackPane::Annotation, ui::TrackPane::Reads] {
            let mut order_index = 0_usize;
            if channel == ui::TrackPane::Annotation && self.reference_engine.is_some() {
                presentation_nodes.push(AutomationPresentationNodeSnapshot {
                    id: PresentationNodeId::Reference,
                    order_index,
                    desired_content_visible: self.track_presentation.reference_content_visible,
                    effective_content_visible: self.track_presentation.reference_content_visible,
                    preferred_height: self.track_presentation.reference_preferred_height,
                    parent_group_id: None,
                    parent_composite_id: None,
                });
                order_index += 1;
            }
            for root in self.track_presentation.pane_root_nodes(channel) {
                match root {
                    PresentationNodeId::Group { group_id, .. } => {
                        if let Some(group) = self
                            .track_presentation
                            .groups
                            .iter()
                            .find(|group| group.channel == channel && group.id == group_id)
                        {
                            presentation_nodes.push(AutomationPresentationNodeSnapshot {
                                id: root,
                                order_index,
                                desired_content_visible: group.content_visible,
                                effective_content_visible: group.content_visible,
                                preferred_height: None,
                                parent_group_id: None,
                                parent_composite_id: None,
                            });
                            order_index += 1;
                            for track_id in &group.members {
                                let node =
                                    self.track_presentation.pane_track_state(channel, *track_id);
                                let desired = node.is_some_and(|node| node.content_visible);
                                let source_available = source_available(*track_id);
                                presentation_nodes.push(AutomationPresentationNodeSnapshot {
                                    id: PresentationNodeId::Track {
                                        channel,
                                        track_id: *track_id,
                                    },
                                    order_index,
                                    desired_content_visible: desired,
                                    effective_content_visible: desired
                                        && source_available
                                        && group.content_visible
                                        && group.expanded
                                        && composite_visible(*track_id),
                                    preferred_height: node.and_then(|node| node.preferred_height),
                                    parent_group_id: Some(group_id),
                                    parent_composite_id: parent_composite(*track_id),
                                });
                                order_index += 1;
                            }
                        }
                    }
                    PresentationNodeId::Track { track_id, .. } => {
                        let node = self.track_presentation.pane_track_state(channel, track_id);
                        let desired = node.is_some_and(|node| node.content_visible);
                        let source_available = source_available(track_id);
                        presentation_nodes.push(AutomationPresentationNodeSnapshot {
                            id: root,
                            order_index,
                            desired_content_visible: desired,
                            effective_content_visible: desired
                                && source_available
                                && composite_visible(track_id),
                            preferred_height: node.and_then(|node| node.preferred_height),
                            parent_group_id: None,
                            parent_composite_id: parent_composite(track_id),
                        });
                        order_index += 1;
                    }
                    PresentationNodeId::Reference => {}
                }
            }
        }
        let to_rect = |rect: egui::Rect| AutomationRectSnapshot {
            left: rect.left(),
            top: rect.top(),
            right: rect.right(),
            bottom: rect.bottom(),
        };
        let pane_layouts = self
            .panels
            .iter()
            .flat_map(|panel| {
                [&panel.pane_layout.annotation, &panel.pane_layout.reads]
                    .into_iter()
                    .map(|layout| AutomationPaneLayoutSnapshot {
                        panel_id: panel.id,
                        channel: layout.channel,
                        pane: to_rect(layout.pane_rect),
                        content_bottom: layout.content_bottom,
                        scroll_points: layout.scroll_points,
                        stack_scroll_points: layout.stack_scroll_points,
                        items: layout
                            .items
                            .iter()
                            .map(|item| AutomationPaneItemLayoutSnapshot {
                                id: item.id,
                                row: to_rect(item.row_rect),
                                header: to_rect(item.header_rect),
                                content: item.content_rect.map(to_rect),
                                clip: to_rect(item.clip_rect),
                                desired_content_visible: item.desired_content_visible,
                                effective_content_visible: item.effective_content_visible,
                                composite_id: match item.frame.key {
                                    TrackFrameKey::Auxiliary(track_id) => {
                                        parent_composite(track_id)
                                    }
                                    TrackFrameKey::Reference
                                    | TrackFrameKey::Alignment(_)
                                    | TrackFrameKey::Group { .. } => None,
                                },
                            })
                            .collect(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let workspace_layout = Some(AutomationWorkspaceLayoutSnapshot {
            revision: self.workspace_layout.revision,
            recomputations: self.workspace_layout_recomputations,
            settled_resize_query_refreshes: self.resize_query_refreshes,
            resolution_query_refresh_pending: self.pending_resolution_query_refresh.is_some(),
            workspace: to_rect(self.workspace_layout.workspace),
            properties: to_rect(self.workspace_layout.properties),
            properties_expanded: self.workspace_layout.properties_expanded,
            properties_resize_active: self.properties_resize_gesture_active,
            display_scale: self.workspace_layout.display_scale,
            accessibility_scale: self.workspace_layout.accessibility_scale,
            genome_context: to_rect(self.workspace_layout.genome_context),
            track_viewport: to_rect(self.workspace_layout.track_viewport),
            context_gutter: to_rect(self.workspace_layout.context_gutter),
            track_gutter: to_rect(self.workspace_layout.track_gutter),
            has_cytoband_row: self.workspace_layout.has_cytoband_row,
            panels: self
                .workspace_layout
                .panels
                .iter()
                .map(|panel| AutomationWorkspacePanelLayoutSnapshot {
                    panel_id: panel.panel_id,
                    cytobands: match panel.cytobands {
                        ui::CytobandAvailability::Loading => {
                            AutomationCytobandAvailability::Loading
                        }
                        ui::CytobandAvailability::Present => {
                            AutomationCytobandAvailability::Present
                        }
                        ui::CytobandAvailability::Absent => AutomationCytobandAvailability::Absent,
                    },
                    column: to_rect(panel.column),
                    header: to_rect(panel.header),
                    header_genomic: to_rect(panel.header_genomic),
                    header_scrollbar: to_rect(panel.header_scrollbar),
                    canvas: to_rect(panel.canvas),
                    scrollbar: to_rect(panel.scrollbar),
                    metadata_row: to_rect(panel.context_rows.metadata),
                    cytoband_row: panel.context_rows.cytoband.map(to_rect),
                    overview_row: to_rect(panel.context_rows.overview),
                    span_row: to_rect(panel.context_rows.span),
                    ruler_row: to_rect(panel.context_rows.ruler),
                })
                .collect(),
        });
        let active_track_gesture = self
            .resizing_annotation_track
            .and_then(|(key, _)| match key {
                TrackFrameKey::Reference => Some(PresentationNodeId::Reference),
                TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
                    Some(PresentationNodeId::Track {
                        channel: ui::TrackPane::Annotation,
                        track_id,
                    })
                }
                TrackFrameKey::Group { channel, id } => Some(PresentationNodeId::Group {
                    channel,
                    group_id: id,
                }),
            })
            .map(|target| AutomationTrackGestureSnapshot {
                kind: AutomationTrackGestureKind::ResizeNode,
                panel_id: self.panels.active().id,
                target: Some(target),
            })
            .or_else(|| {
                self.resizing_read_track
                    .map(|(track_id, _)| AutomationTrackGestureSnapshot {
                        kind: AutomationTrackGestureKind::ResizeNode,
                        panel_id: self.panels.active().id,
                        target: Some(PresentationNodeId::Track {
                            channel: ui::TrackPane::Reads,
                            track_id,
                        }),
                    })
            })
            .or_else(|| {
                self.resizing_read_pane
                    .map(|_| AutomationTrackGestureSnapshot {
                        kind: AutomationTrackGestureKind::ResizeReadPane,
                        panel_id: self.panels.active().id,
                        target: None,
                    })
            })
            .or_else(|| {
                self.active_stack_scrollbar_gesture
                    .map(|(panel_id, channel)| AutomationTrackGestureSnapshot {
                        kind: match channel {
                            ui::TrackPane::Annotation => {
                                AutomationTrackGestureKind::AnnotationScrollbar
                            }
                            ui::TrackPane::Reads => AutomationTrackGestureKind::ReadStackScrollbar,
                        },
                        panel_id,
                        target: None,
                    })
            })
            .or_else(|| {
                self.vertical_overview_drag
                    .map(|drag| AutomationTrackGestureSnapshot {
                        kind: AutomationTrackGestureKind::ReadScrollbar,
                        panel_id: drag.panel_id,
                        target: Some(PresentationNodeId::Track {
                            channel: ui::TrackPane::Reads,
                            track_id: drag.track_id,
                        }),
                    })
            });
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
                context_label: self.assembly.context_label(),
                assembly_identity: self.assembly_identity.clone(),
                suggested_assembly_identity: self.suggested_assembly_identity.clone(),
                reference_configured: self.reference_engine.is_some(),
                reference_ready: self.reference_engine.is_some()
                    && self.reference_dictionary.is_some(),
                reference_contig_count: self.assembly.reference_contig_count,
                navigation_contig_count: self.assembly.dictionary.len(),
                reference_is_subset: self.assembly_identity.as_ref().is_some_and(|identity| {
                    identity.expected_primary_contigs > 0
                        && self.assembly.reference_contig_count < identity.expected_primary_contigs
                }),
                cytobands_loaded: self.assembly.has_cytobands(),
                default_annotation_count: self.assembly.default_annotation_count,
                gene_model_track_count: self
                    .tracks
                    .tracks()
                    .iter()
                    .filter(|track| track.kind == TrackKind::GeneModel)
                    .count(),
                contig_alias_count: self.contig_aliases.len(),
                landmarks: self.automation_landmark_snapshot(),
            },
            settled: self.automation_data_is_settled(),
            active_panel_id: self.panels.active().id,
            panels,
            tracks,
            read_pane_height: self.track_presentation.read_pane_height,
            annotation_section_expanded: self.track_presentation.annotation_section_expanded,
            read_section_expanded: self.track_presentation.read_section_expanded,
            presentation_groups: self
                .track_presentation
                .groups
                .iter()
                .map(|group| AutomationPresentationGroupSnapshot {
                    channel: group.channel,
                    id: group.id,
                    label: group.label.clone(),
                    members: group.members.clone(),
                    expanded: group.expanded,
                    content_visible: group.content_visible,
                })
                .collect(),
            presentation_composites: self
                .track_presentation
                .composites
                .iter()
                .map(|composite| AutomationPresentationCompositeSnapshot {
                    channel: composite.channel,
                    id: composite.id,
                    content_visible: composite.content_visible,
                })
                .collect(),
            presentation_nodes,
            pane_layouts,
            workspace_layout,
            active_track_gesture,
            selection: self.automation_selection_snapshot(),
            // Free-form status strings can contain local paths. Typed lifecycle
            // and readiness fields above provide safe status instead.
            status: Vec::new(),
            truncated,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn automation_selection_snapshot(&self) -> Option<AutomationSelectionSnapshot> {
        if !self
            .panels
            .active()
            .navigation_presentation
            .is_interactive()
        {
            return None;
        }
        Some(observed_retained_selection(
            &self.panels.active().retained_evidence.as_ref()?.selection,
            self.panels.active().viewport.interval(),
            &self.panels.active().contig_name,
        ))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn live_automation_selection_snapshot(&self) -> Option<AutomationSelectionSnapshot> {
        const MAXIMUM_FIELD_KEY_BYTES: usize = 256;
        const MAXIMUM_FIELD_VALUE_BYTES: usize = 4_096;

        let panel = self.panels.active();
        if !panel.navigation_presentation.is_interactive() {
            return None;
        }
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
                contig: panel.contig_name.clone(),
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
                visible_in_active_view: start.zip(end).is_some_and(|(start, end)| {
                    GenomicInterval::new(panel.viewport.interval().contig(), start, end)
                        .is_ok_and(|evidence| panel.viewport.interval().contains_interval(evidence))
                }),
                stale: false,
                stale_reason: None,
                truncated,
                presentation_truncated: false,
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
                inspector
                    .document(format!("pileup:{position}"), "Pileup")
                    .flatten_automation_fields(),
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
                inspector
                    .document(format!("modification:{position}"), "Modification frequency")
                    .flatten_automation_fields(),
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
                inspector
                    .document(format!("junction:{start}:{end}"), "Splice junction")
                    .flatten_automation_fields(),
            ));
        }
        if let Some(index) = panel.selected_record
            && let Some(inspector) = self.inspector_snapshot()
        {
            let track_id = alignment_track.unwrap_or_else(|| self.primary_alignment_track_id());
            let query = self.complete_read_query_for_track(track_id)?;
            let record = query.materialize_record(index).ok()?;
            let fields = inspector
                .document(
                    format!("read:{}:{index}", track_id.get()),
                    "Alignment segment",
                )
                .flatten_automation_fields()
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
            Some(TrackFrameKey::Group { .. }) => None,
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
        selection.presentation_truncated = inspector
            .as_ref()
            .and_then(|inspector| inspector.cigar.as_ref())
            .is_some_and(|cigar| cigar.len() > 256);
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
                        let visible = automation_bool(value, key)?;
                        self.apply_track_intents(&[TrackIntent::SetNodeVisibility {
                            target: PresentationNodeId::Track {
                                channel: ui::TrackPane::Reads,
                                track_id: self.primary_alignment_track_id(),
                            },
                            visible,
                        }])?;
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
                    "include_secondary" => {
                        let requested = automation_bool(value, key)?;
                        query_changed =
                            requested != self.primary_active_filters().include_secondary;
                        self.primary_alignment_filters_mut().include_secondary = requested;
                        self.primary_active_filters_mut().include_secondary = requested;
                        self.primary_filter_editor_mut().include_secondary = requested;
                    }
                    "include_supplementary" => {
                        let requested = automation_bool(value, key)?;
                        query_changed =
                            requested != self.primary_active_filters().include_supplementary;
                        self.primary_alignment_filters_mut().include_supplementary = requested;
                        self.primary_active_filters_mut().include_supplementary = requested;
                        self.primary_filter_editor_mut().include_supplementary = requested;
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
                        let track_id = self.primary_alignment_track_id();
                        self.set_alignment_density_for_track(track_id, density);
                    }
                    "coverage_scale_maximum" => {
                        let maximum = automation_optional_u64(value, key)?
                            .map(|value| automation_u32(value, key))
                            .transpose()?;
                        anyhow::ensure!(maximum != Some(0), "{key} must be positive or null");
                        self.primary_alignment_filters_mut().coverage_scale_maximum = maximum;
                    }
                    "coverage_scale_transform" => {
                        let requested = automation_name(value, key)?;
                        self.primary_alignment_filters_mut()
                            .coverage_scale_transform = match requested.as_str() {
                            "linear" => CoverageScaleTransform::Linear,
                            "square_root" => CoverageScaleTransform::SquareRoot,
                            _ => anyhow::bail!("{key} must be linear or square_root"),
                        };
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
                let source_exists = self
                    .alignment_tracks
                    .iter()
                    .any(|track| track.config.id == track_id)
                    || self
                        .tracks
                        .tracks()
                        .iter()
                        .any(|track| track.id == track_id);
                anyhow::ensure!(source_exists, "track {} was not found", track_id.get());
                let presentation_intent = match key {
                    "visible" => Some(TrackIntent::SetNodeVisibility {
                        target: PresentationNodeId::Track {
                            channel: ui::TrackPane::Annotation,
                            track_id,
                        },
                        visible: automation_bool(value, key)?,
                    }),
                    "collapsed" => Some(TrackIntent::SetNodeVisibility {
                        target: PresentationNodeId::Track {
                            channel: ui::TrackPane::Annotation,
                            track_id,
                        },
                        visible: !automation_bool(value, key)?,
                    }),
                    "height" => Some(TrackIntent::ResizeNode {
                        target: PresentationNodeId::Track {
                            channel: ui::TrackPane::Annotation,
                            track_id,
                        },
                        preferred_height: automation_f32(value, key)?,
                    }),
                    _ => None,
                };
                if let Some(intent) = presentation_intent {
                    self.apply_track_intents(&[intent])?;
                    self.rebuild_all_panel_displays();
                    self.display_dirty = true;
                    return Ok(());
                }
                let primary_id = self.primary_alignment_track_id();
                if let Some(track) = self
                    .alignment_tracks
                    .iter_mut()
                    .find(|track| track.config.id == track_id)
                {
                    match key {
                        "name" => track.config.name = automation_name(value, key)?,
                        "sample_key" => {
                            track.config.sample_key = automation_optional_name(value, key)?;
                        }
                        _ => anyhow::bail!("unknown alignment-track option {key}"),
                    }
                    if track_id == primary_id {
                        self.activate_first_alignment_track()?;
                    }
                    self.rebuild_all_panel_displays();
                } else if let Some(track) = self
                    .tracks
                    .tracks_mut()
                    .iter_mut()
                    .find(|track| track.id == track_id)
                {
                    match key {
                        "name" => track.name = automation_name(value, key)?,
                        "sample_key" => {
                            track.sample_key = automation_optional_name(value, key)?;
                        }
                        _ => anyhow::bail!("unknown auxiliary-track option {key}"),
                    }
                    self.rebuild_all_panel_displays();
                }
            }
            AutomationOptionScope::Panel { panel_id } => {
                let primary_track_id = self.primary_alignment_track_id();
                if self.panels.get(panel_id).is_none() {
                    anyhow::bail!("panel {} was not found", panel_id.get());
                }
                match key {
                    "vertical_scroll_points" => {
                        let scroll = automation_f32(value, key)?;
                        anyhow::ensure!(scroll >= 0.0, "{key} must be non-negative");
                        let panel = self
                            .panels
                            .get_mut(panel_id)
                            .expect("panel existence was checked above");
                        let maximum = panel
                            .scroll_display_cache
                            .as_ref()
                            .and_then(|cache| {
                                cache
                                    .read_panels
                                    .iter()
                                    .find(|read_panel| read_panel.track_id == primary_track_id)
                            })
                            .map_or(0.0, |read_panel| {
                                (panel
                                    .read_panel_scroll_state(primary_track_id)
                                    .content_bottom_points
                                    - read_panel.clip_bottom)
                                    .max(0.0)
                            });
                        let scroll = scroll.min(maximum);
                        panel.vertical_scroll_points = scroll;
                        panel
                            .read_panel_scrolls
                            .entry(primary_track_id)
                            .or_default()
                            .scroll_points = scroll;
                    }
                    "annotation_scroll_points" => {
                        let scroll = automation_f32(value, key)?;
                        anyhow::ensure!(scroll >= 0.0, "{key} must be non-negative");
                        self.panels.annotation_scroll_points =
                            scroll.min(self.panels.maximum_annotation_scroll_points());
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
        let mut deferred_shell_change = false;
        match setting {
            AutomationSetting::WindowSize { width, height } => {
                let current = self
                    .window
                    .inner_size()
                    .to_logical::<f64>(self.window.scale_factor());
                deferred_shell_change = (current.width - f64::from(*width)).abs() >= 0.5
                    || (current.height - f64::from(*height)).abs() >= 0.5;
                let _ = self.window.request_inner_size(winit::dpi::LogicalSize::new(
                    f64::from(*width),
                    f64::from(*height),
                ));
                self.window.request_redraw();
            }
            AutomationSetting::InterfaceScalePercent { value } => {
                let scale = f32::from(*value) / 100.0;
                deferred_shell_change = (self.egui_context.zoom_factor() - scale).abs() > 0.001;
                self.egui_context.set_zoom_factor(scale);
                self.egui_context.request_repaint();
                self.window.request_redraw();
            }
            AutomationSetting::PropertiesPanelExpanded { value } => {
                let id = egui::Id::new("properties-panel-expanded");
                let current = self
                    .egui_context
                    .data(|data| data.get_temp::<bool>(id))
                    .unwrap_or(true);
                deferred_shell_change = current != *value;
                self.egui_context
                    .data_mut(|data| data.insert_temp(id, *value));
                self.egui_context.request_repaint();
                self.window.request_redraw();
            }
            AutomationSetting::PropertiesPanelWidth { value } => {
                let id = egui::Id::new("properties-panel");
                let current = egui::containers::panel::PanelState::load(&self.egui_context, id)
                    .map_or(340.0, |state| state.outer_rect.width());
                deferred_shell_change = (current - value).abs() >= 0.5;
                let rect = egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(*value, self.workspace_layout.properties.height().max(1.0)),
                );
                self.egui_context.data_mut(|data| {
                    data.insert_persisted(
                        id,
                        egui::containers::panel::PanelState { outer_rect: rect },
                    );
                });
                self.egui_context.request_repaint();
                self.window.request_redraw();
            }
            AutomationSetting::PropertiesPanelResizeActive { value } => {
                deferred_shell_change = self.automation_properties_resize_active != *value;
                self.automation_properties_resize_active = *value;
                if *value {
                    // A held sash may pause long enough for inspection or a
                    // native capture. Gesture lifecycle, not elapsed wall
                    // time, decides when the final resolution query is due.
                    self.pending_resolution_query_refresh = None;
                } else if deferred_shell_change {
                    self.schedule_resolution_query_refresh();
                }
                self.egui_context.request_repaint();
                self.window.request_redraw();
            }
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
            AutomationSetting::IncludeSecondary { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "include_secondary",
                &serde_json::json!(value),
            )?,
            AutomationSetting::IncludeSupplementary { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "include_supplementary",
                &serde_json::json!(value),
            )?,
            AutomationSetting::CoverageScaleTransform { value } => self.apply_automation_option(
                &AutomationOptionScope::Workspace,
                "coverage_scale_transform",
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
            AutomationSetting::ReadStackingMode { track_ids, value } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    track.config.filters.read_stacking_mode = *value;
                }
                self.invalidate_automation_alignment_layouts(Some(track_ids), false);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::CompleteReadLimit { track_ids, value } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                let (workspace_limit, requested_limit) =
                    expanded_workspace_alignment_limit(self.workspace_alignment_limit, *value);
                self.workspace_alignment_limit = workspace_limit;
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    track.config.maximum_alignments = requested_limit;
                }
                self.invalidate_automation_alignment_layouts(Some(track_ids), true);
                self.request_all_panel_queries();
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::RebuildStableReadArchitecture {
                panel_id,
                track_ids,
            } => {
                self.ensure_automation_alignment_tracks(track_ids)?;
                anyhow::ensure!(
                    self.panels.get(*panel_id).is_some(),
                    "panel {} was not found",
                    panel_id.get()
                );
                for track in self
                    .alignment_tracks
                    .iter_mut()
                    .filter(|track| track_ids.contains(&track.config.id))
                {
                    anyhow::ensure!(
                        track.config.filters.read_stacking_mode == ReadStackingMode::Stable,
                        "Rebuild stable architecture requires stable read stacking for track {}",
                        track.config.id.get()
                    );
                    let projection = track.projection_mut(*panel_id);
                    projection.pending_row_repack = true;
                    projection.alignment_rows = None;
                    projection.cached_layout = None;
                }
                self.layout_cache.clear();
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationSectionExpanded { channel, value } => {
                self.apply_track_intents(&[TrackIntent::SetSectionExpanded {
                    channel: *channel,
                    expanded: *value,
                }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationNodeVisible { target, value } => {
                self.apply_track_intents(&[TrackIntent::SetNodeVisibility {
                    target: *target,
                    visible: *value,
                }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationNodeHeight { target, value } => {
                self.apply_track_intents(&[TrackIntent::ResizeNode {
                    target: *target,
                    preferred_height: *value,
                }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationGroupExpanded { target, value } => {
                self.apply_track_intents(&[TrackIntent::SetGroupExpanded {
                    target: *target,
                    expanded: *value,
                }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationGroupVisible { target, value } => {
                self.apply_track_intents(&[TrackIntent::SetGroupVisibility {
                    target: *target,
                    visible: *value,
                }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationCompositeVisible {
                channel,
                composite_id,
                value,
            } => {
                self.apply_track_intents(&[TrackIntent::SetCompositeVisibility {
                    channel: *channel,
                    composite_id: *composite_id,
                    visible: *value,
                }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationGroup { channel, track_ids } => {
                self.apply_track_intents(&[TrackIntent::GroupNodes {
                    channel: *channel,
                    members: track_ids.clone(),
                }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::PresentationUngroup { target } => {
                self.apply_track_intents(&[TrackIntent::Ungroup { target: *target }])?;
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ReadPaneHeight { value } => {
                self.apply_track_intents(&[TrackIntent::ResizeReadPane { height: *value }])?;
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
            AutomationSetting::AnnotationScrollPoints { panel_id, value } => self
                .apply_automation_option(
                    &AutomationOptionScope::Panel {
                        panel_id: *panel_id,
                    },
                    "annotation_scroll_points",
                    &serde_json::json!(value),
                )?,
            AutomationSetting::ReadStackScrollPoints { panel_id, value } => {
                let Some(panel) = self.panels.get_mut(*panel_id) else {
                    anyhow::bail!("panel {} was not found", panel_id.get());
                };
                let maximum = (panel.pane_layout.reads.content_bottom
                    - panel.pane_layout.reads.pane_rect.bottom())
                .max(0.0);
                panel.read_stack_scroll_points = value.clamp(0.0, maximum);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ReadPanelScrollPoints {
                panel_id,
                track_id,
                value,
            } => {
                self.ensure_automation_alignment_tracks(&[*track_id])?;
                let Some(panel) = self.panels.get_mut(*panel_id) else {
                    anyhow::bail!("panel {} was not found", panel_id.get());
                };
                let maximum = panel
                    .scroll_display_cache
                    .as_ref()
                    .and_then(|cache| {
                        cache
                            .read_panels
                            .iter()
                            .find(|read_panel| read_panel.track_id == *track_id)
                    })
                    .map_or(0.0, |read_panel| {
                        (panel
                            .read_panel_scroll_state(*track_id)
                            .content_bottom_points
                            - read_panel.clip_bottom)
                            .max(0.0)
                    });
                panel
                    .read_panel_scrolls
                    .entry(*track_id)
                    .or_default()
                    .scroll_points = value.clamp(0.0, maximum);
                self.rebuild_all_panel_displays();
            }
            AutomationSetting::ReadPanelDensity {
                panel_id,
                track_id,
                value,
            } => {
                self.ensure_automation_alignment_tracks(&[*track_id])?;
                anyhow::ensure!(
                    self.panels.get(*panel_id).is_some(),
                    "panel {} was not found",
                    panel_id.get()
                );
                self.set_read_panel_density(*panel_id, *track_id, *value);
                self.invalidate_automation_alignment_layouts(Some(&[*track_id]), false);
                self.rebuild_all_panel_displays();
            }
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
            AutomationSetting::AnnotationScrollPoints { panel_id, .. } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| serde_json::json!(panel.annotation_scroll_points)),
            AutomationSetting::ReadStackScrollPoints { panel_id, .. } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .map(|panel| serde_json::json!(panel.read_stack_scroll_points)),
            AutomationSetting::ReadPanelScrollPoints {
                panel_id, track_id, ..
            } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .and_then(|panel| {
                    panel
                        .read_panel_states
                        .iter()
                        .find(|state| state.track_id == *track_id)
                })
                .map(|state| serde_json::json!(state.scroll_points)),
            AutomationSetting::ReadPanelDensity {
                panel_id, track_id, ..
            } => workspace
                .panels
                .iter()
                .find(|panel| panel.id == *panel_id)
                .and_then(|panel| {
                    panel
                        .read_panel_states
                        .iter()
                        .find(|state| state.track_id == *track_id)
                })
                .map(|state| serde_json::json!(state.effective_density)),
            AutomationSetting::TrackHeight { track_id, .. } => workspace
                .tracks
                .iter()
                .find(|track| track.id == *track_id)
                .map(|track| serde_json::json!(track.height)),
            AutomationSetting::CompleteReadLimit { track_ids, .. } => workspace
                .tracks
                .iter()
                .find(|track| track_ids.contains(&track.id))
                .and_then(|track| track.alignment.as_ref())
                .map(|alignment| serde_json::json!(alignment.requested_complete_read_limit)),
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
            "changed": before != after || deferred_shell_change,
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
        let presentation_committed = self
            .panels
            .get(panel_id)
            .is_some_and(|panel| panel.navigation_presentation.is_interactive());
        let track = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .with_context(|| format!("alignment track {} was not found", track_id.get()))?;
        let projection = track
            .projection(panel_id)
            .context("the alignment query is unavailable for this panel")?;
        let generation = projection.clock.current().get();
        let query = automation_complete_alignment_query(track, panel_id, presentation_committed)?;
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
            let modification_result = if matches!(record.sequence, SequenceData::NotLoaded)
                && record.auxiliary_fields.is_empty()
            {
                None
            } else {
                Some(decode_modified_bases(&record))
            };
            let modifications = modification_result
                .as_ref()
                .and_then(|result| result.as_ref().ok());
            let modification_evidence_status = match &modification_result {
                None => "unavailable",
                Some(Ok(calls)) if calls.is_empty() => "absent",
                Some(Ok(_)) => "present",
                Some(Err(_)) => "malformed",
            };
            if matches!(
                selector,
                AutomationAlignmentSelector::Semantic {
                    has_modified_bases: Some(_),
                    ..
                }
            ) && modification_evidence_status == "unavailable"
            {
                anyhow::bail!(
                    "modified-base evidence is unavailable at the current query detail; enable modified-base display or zoom to read detail"
                );
            }
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
                        && has_modified_bases.is_none_or(|expected| {
                            modifications.is_some_and(|calls| expected != calls.is_empty())
                        })
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
                "contig": self.panels
                    .iter()
                    .find(|panel| panel.id == panel_id)
                    .map(|panel| panel.contig_name.as_str())
                    .unwrap_or_default(),
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
                "modification_code_count": modifications.map_or(0, |calls| {
                    calls.iter().map(|call| call.code).collect::<HashSet<_>>().len()
                }),
                "modification_evidence_status": modification_evidence_status,
            }));
        }
        Ok(serde_json::json!({
            "application_instance_id": self.application_instance_id,
            "workspace_revision": self.workspace_revision,
            "query_generation": generation,
            "query_contig": self.panels
                .iter()
                .find(|panel| panel.id == panel_id)
                .map(|panel| panel.contig_name.as_str())
                .unwrap_or_default(),
            "query_start": query.interval.start(),
            "query_end": query.interval.end(),
            "hits": hits,
            "truncated": matched_count > maximum_results,
        }))
    }

    pub(crate) fn select_automation_alignment(
        &mut self,
        handle: &str,
        reveal: AutomationAlignmentReveal,
    ) -> Result<serde_json::Value> {
        let target = self
            .automation_alignment_handles
            .get(handle)
            .copied()
            .context("alignment handle is stale or unknown")?;
        let presentation_committed = self
            .panels
            .get(target.panel_id)
            .is_some_and(|panel| panel.navigation_presentation.is_interactive());
        let track = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == target.track_id)
            .context("alignment handle track is unavailable")?;
        anyhow::ensure!(
            track.projection(target.panel_id).is_some_and(|projection| {
                projection.clock.current().get() == target.query_generation
            }),
            "alignment handle is stale"
        );
        let query =
            automation_complete_alignment_query(track, target.panel_id, presentation_committed)?;
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
        replace_panel_track_selection(
            self.panels.active_mut(),
            TrackFrameKey::Alignment(target.track_id),
            ui::TrackPane::Reads,
        );
        self.panels.active_mut().selected_record = Some(target.record_index);
        self.panels.active_mut().selected_chain = chain;
        self.panels.active_mut().pending_read_selection =
            PendingReadSelection::for_record(target.track_id, &record);
        self.bump_workspace_revision();
        self.retain_active_selection(None);
        self.rebuild_genomic_display();
        let mut navigated = false;
        if reveal != AutomationAlignmentReveal::None {
            let operation = match reveal {
                AutomationAlignmentReveal::None => unreachable!(),
                AutomationAlignmentReveal::Fit => AutomationViewportOperation::FitInterval {
                    start: record.start,
                    end: record.end,
                },
                AutomationAlignmentReveal::Center => {
                    let span = self.panels.active().viewport.interval().len();
                    let midpoint = record.start.saturating_add(record.end) / 2;
                    let start = midpoint.saturating_sub(span / 2);
                    AutomationViewportOperation::FitInterval {
                        start,
                        end: start.saturating_add(span),
                    }
                }
            };
            self.apply_automation_viewport_operation(target.panel_id, operation)?;
            self.panels.active_mut().pending_read_selection =
                PendingReadSelection::for_record(target.track_id, &record);
            navigated = true;
        }
        let interval = self.panels.active().viewport.interval();
        Ok(serde_json::json!({
            "selection": self.automation_selection_snapshot(),
            "reveal": format!("{reveal:?}").to_ascii_lowercase(),
            "navigated": navigated,
            "viewport": {
                "contig": self.panels.active().contig_name,
                "start": interval.start(),
                "end": interval.end(),
            },
            "workspace_revision": self.workspace_revision,
        }))
    }

    pub(crate) fn automation_related_alignments(&self) -> Result<serde_json::Value> {
        let presentation_committed = self
            .panels
            .active()
            .navigation_presentation
            .is_interactive();
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
        let track = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .context("selected alignment track is unavailable")?;
        let query = automation_complete_alignment_query(
            track,
            self.panels.active().id,
            presentation_committed,
        )?;
        let record = query.materialize_record(index)?;
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
                let (pending_selection, locus) = self
                    .selected_mate_target()
                    .context("the selected alignment has no mapped mate")?;
                let track_id = pending_selection.track_id;
                self.navigate_to_locus(&locus)?;
                let panel = self.panels.active_mut();
                panel.pending_read_selection = Some(pending_selection);
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
                    "anchor": track.config.filters.alignment_sort_anchor,
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
        let captured_anchor = alignment_sort_position(mode, None, self.panels.active().viewport);
        for track in self
            .alignment_tracks
            .iter_mut()
            .filter(|track| track_ids.contains(&track.config.id))
        {
            track.config.filters.alignment_sort_mode = mode;
            track.config.filters.alignment_sort_order = direction;
            track.config.filters.alignment_sort_anchor = captured_anchor;
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
                    projection.advance_query_generation();
                    projection.pending_query =
                        !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_));
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
                self.select_track_key(key, ui::TrackPane::Annotation);
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
                            TrackSnapshotData::Features(records) => records
                                .iter()
                                .enumerate()
                                .filter(|(_, record)| {
                                    record.start <= position && position < record.end
                                })
                                .min_by_key(|(index, record)| {
                                    (record.end.saturating_sub(record.start), *index)
                                })
                                .map(|(index, _)| (Some(index), None, None)),
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
                                .map(|(index, _)| (None, Some(index), None)),
                            TrackSnapshotData::Quantitative(query) => {
                                quantitative_record_index_at_position(&query.records, position)
                                    .map(|index| (None, None, Some(index)))
                            }
                            TrackSnapshotData::CopyNumber(_)
                            | TrackSnapshotData::Mutations(_)
                            | TrackSnapshotData::Interactions(_)
                            | TrackSnapshotData::Gwas(_) => None,
                        })
                } else {
                    None
                };
                self.panels.active_mut().clear_selection();
                if matches!(key, TrackFrameKey::Alignment(_)) {
                    self.panels.active_mut().selected_alignment_track = Some(track_id);
                    replace_panel_track_selection(
                        self.panels.active_mut(),
                        key,
                        ui::TrackPane::Annotation,
                    );
                    self.panels.active_mut().selected_pileup_position = Some(position);
                } else if let Some((feature, variant, quantitative)) = auxiliary_selection {
                    replace_panel_track_selection(
                        self.panels.active_mut(),
                        key,
                        ui::TrackPane::Annotation,
                    );
                    self.panels.active_mut().selected_feature =
                        feature.map(|record_index| FeatureSelection {
                            track_id,
                            record_index,
                        });
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
            .filter(|projection| projection.query_is_current())
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

    #[allow(clippy::too_many_lines)]
    pub(crate) fn start_automation_snapshot(
        &mut self,
        command_index: usize,
        path: PathBuf,
        scope: AutomationSnapshotScope,
        format: AutomationSnapshotFormat,
    ) -> Result<()> {
        if scope == AutomationSnapshotScope::NativeWindow {
            anyhow::ensure!(
                format == AutomationSnapshotFormat::Png,
                "native window snapshots support PNG only"
            );
            anyhow::ensure!(
                self.pending_native_window_capture.is_none(),
                "a native window capture is already in progress"
            );
            anyhow::ensure!(
                !self.export_tasks.iter().any(|task| task.path == path),
                "an export to {} is already in progress",
                path.display()
            );
            let width = self.surface_config.width;
            let height = self.surface_config.height;
            let navigation_preview_active = self
                .panels
                .iter()
                .any(|panel| panel.navigation_presentation.shows_retained_preview());
            let navigation_transaction_active = self
                .panels
                .iter()
                .any(|panel| panel.navigation_presentation.is_unsettled());
            self.pending_native_window_capture = Some(NativeWindowCaptureTask {
                path,
                automation_command: command_index,
                automation_data: serde_json::json!({
                    "width": width,
                    "height": height,
                    "scope": scope,
                    "scope_description": "native window client area",
                    "truncated": false,
                    "application_instance_id": self.application_instance_id,
                    "workspace_revision": self.workspace_revision,
                    "navigation_preview_active": navigation_preview_active,
                    "navigation_transaction_active": navigation_transaction_active,
                    "selection_id": self.automation_selection_snapshot().map(|selection| selection.selection_id),
                    "evidence_revision": self.automation_selection_snapshot().map(|selection| selection.evidence_revision),
                }),
            });
            self.display_dirty = true;
            self.window.request_redraw();
            return Ok(());
        }
        let requested_presentations_committed = match scope {
            AutomationSnapshotScope::VisibleWorkspace => self
                .panels
                .iter()
                .all(|panel| panel.navigation_presentation.is_interactive()),
            AutomationSnapshotScope::ActivePanel
            | AutomationSnapshotScope::FullActivePanel
            | AutomationSnapshotScope::NativeWindow => self
                .panels
                .active()
                .navigation_presentation
                .is_interactive(),
        };
        anyhow::ensure!(
            requested_presentations_committed,
            "the target viewport is still being checked"
        );
        let (display, width, height, scope_label) = match scope {
            AutomationSnapshotScope::VisibleWorkspace => {
                let bounds = self.workspace_layout.workspace;
                let width = bounds.width().ceil().max(1.0) as u32;
                let height = bounds.height().ceil().max(1.0) as u32;
                (
                    cropped_display_list(&self.composite_display_list, bounds),
                    width,
                    height,
                    format!(
                        "visible workspace; committed layout revision {}",
                        self.workspace_layout.revision
                    ),
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
            AutomationSnapshotScope::NativeWindow => unreachable!(),
        };
        let display = semantic_export_display_list(&display);
        let mut metadata = automation_snapshot_metadata(
            &self.assembly_name,
            &self.panels,
            &self.alignment_tracks,
            &self.tracks,
            &self.track_presentation,
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
        let original_workspace_layout = self.workspace_layout.clone();
        let original_scroll = self.panels.active().vertical_scroll_points;
        let original_content_origin = self.panels.active().vertical_content_origin_points;
        let original_read_panel_scrolls = self.panels.active().read_panel_scrolls.clone();
        let content_height = (self.panels.active().content_bottom_points
            - self.canvas_geometry().viewport.top())
        .ceil()
        .max(1.0);
        let export_height = content_height.min(MAX_AUTOMATION_FULL_PANEL_HEIGHT as f32) as u32;
        let truncated = content_height > MAX_AUTOMATION_FULL_PANEL_HEIGHT as f32;
        let expanded_workspace = egui::Rect::from_min_max(
            original_workspace_layout.workspace.min,
            egui::pos2(
                original_workspace_layout.workspace.right(),
                original_workspace_layout.genome_context.bottom() + export_height as f32,
            ),
        );
        let panel_inputs = original_workspace_layout
            .panels
            .iter()
            .map(|panel| WorkspacePanelLayoutInput {
                panel_id: panel.panel_id,
                contig_name: panel.contig_name.clone(),
                contig_length: panel.contig_length,
                cytobands: panel.cytobands,
            })
            .collect::<Vec<_>>();
        self.workspace_layout = WorkspaceLayoutSnapshot::compute(
            WorkspaceLayoutInput {
                workspace: expanded_workspace,
                properties: original_workspace_layout.properties,
                properties_expanded: original_workspace_layout.properties_expanded,
                panels: panel_inputs,
                metrics: original_workspace_layout.metrics,
                requested_track_gutter_width: original_workspace_layout
                    .requested_track_gutter_width,
                minimum_track_gutter_width: original_workspace_layout.minimum_track_gutter_width,
                requested_scrollbar_width: original_workspace_layout.requested_scrollbar_width,
                minimum_scrollbar_width: original_workspace_layout.minimum_scrollbar_width,
                display_scale: original_workspace_layout.display_scale,
                accessibility_scale: original_workspace_layout.accessibility_scale,
            },
            original_workspace_layout.revision,
        );
        self.panels.active_mut().vertical_scroll_points = 0.0;
        self.panels.active_mut().vertical_content_origin_points = 0.0;
        for state in self.panels.active_mut().read_panel_scrolls.values_mut() {
            state.scroll_points = 0.0;
            state.content_origin_points = 0.0;
        }
        self.rebuild_genomic_display();
        let bounds = self.active_panel_rect();
        let display = cropped_display_list(&self.panels.active().display_list, bounds);
        let width = bounds.width().ceil().max(1.0) as u32;

        self.workspace_layout = original_workspace_layout;
        self.panels.active_mut().vertical_scroll_points = original_scroll;
        self.panels.active_mut().vertical_content_origin_points = original_content_origin;
        self.panels.active_mut().read_panel_scrolls = original_read_panel_scrolls;
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
                    &self.track_presentation,
                );
                self.start_automation_export_job(command_index, path, move |path| {
                    std::fs::write(path, contents)
                })
            }
            AutomationExportKind::Alignments
            | AutomationExportKind::Pileup
            | AutomationExportKind::Modifications
            | AutomationExportKind::Junctions => {
                let presentation_committed = self
                    .panels
                    .active()
                    .navigation_presentation
                    .is_interactive();
                anyhow::ensure!(
                    presentation_committed,
                    "the target viewport is still being checked"
                );
                let mut tracks = Vec::new();
                for track in self.alignment_tracks.iter().filter(|track| {
                    self.track_presentation
                        .track_content_exposed(ui::TrackPane::Annotation, track.config.id)
                        .unwrap_or(false)
                        || self
                            .track_presentation
                            .track_content_exposed(ui::TrackPane::Reads, track.config.id)
                            .unwrap_or(false)
                }) {
                    let query = if kind == AutomationExportKind::Alignments {
                        Some(automation_complete_alignment_query(
                            track,
                            self.panels.active().id,
                            presentation_committed,
                        )?)
                    } else {
                        track
                            .projection(self.panels.active().id)
                            .filter(|projection| projection.query_is_current())
                            .and_then(|projection| projection.current_query.as_ref())
                            .cloned()
                    };
                    if let Some(query) = query {
                        tracks.push(AutomationAlignmentExportTrack {
                            id: track.config.id,
                            name: track.config.name.clone(),
                            query,
                        });
                    }
                }
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
                    .filter(|track| {
                        self.track_presentation
                            .track_content_exposed(ui::TrackPane::Annotation, track.id)
                            .unwrap_or(false)
                    })
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
