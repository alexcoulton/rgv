use crate::Viewer;
use crate::automation::{
    AutomationAuxiliaryOpenState, AutomationExecution, AutomationNavigationState, AutomationRunner,
    AutomationWait, atomic_write_text,
};
use crate::control_service::ControlService;
use crate::workspace::alignments::AlignmentTrackLifecycle;
use crate::workspace::queries::reference_dependency_is_settled;
use rgv_automation::{
    AutomationCommand, AutomationErrorCode, AutomationSelection, AutomationSelectionGranularity,
};
use rgv_core::{MAXIMUM_BASE_MARK_BASES_PER_PIXEL, PanelId};
use std::time::{Duration, Instant};

/// Longest a batch waits for a snapshot, export or save worker before failing the command.
pub(crate) const AUTOMATION_FILE_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

impl Viewer {
    pub(crate) fn automation_navigation_state(
        &self,
        panel_id: PanelId,
    ) -> AutomationNavigationState {
        let mut state = AutomationNavigationState::default();
        let mut phases = Vec::new();
        for track in &self.alignment_tracks {
            if track.scene_engine.is_none() {
                continue;
            }
            let Some(projection) = track.projection(panel_id) else {
                continue;
            };
            let telemetry = projection.scene_telemetry;
            state.source_acquisitions = state
                .source_acquisitions
                .saturating_add(telemetry.source_acquisitions);
            state.foreground_submissions = state
                .foreground_submissions
                .saturating_add(telemetry.foreground_submissions);
            state.speculation_submissions = state
                .speculation_submissions
                .saturating_add(telemetry.speculation_submissions);
            state.camera_only_updates = state
                .camera_only_updates
                .saturating_add(telemetry.camera_only_updates);
            state.scene_publications = state
                .scene_publications
                .saturating_add(telemetry.scene_publications);
            phases.push(format!("{:?}", projection.navigation.phase()).to_ascii_lowercase());
        }
        state.scene_phase = phases.join(",");
        state
    }

    pub(crate) fn automation_is_settled(&self) -> bool {
        !self.display_dirty && self.next_egui_repaint.is_none() && self.automation_data_is_settled()
    }

    /// Whether the observable workspace data has settled. Receiving a local-control
    /// request dirties the status bar before its first command runs, so observation
    /// snapshots must not treat that control-only repaint as unsettled genomic data.
    pub(super) fn automation_data_is_settled(&self) -> bool {
        if !self.export_tasks.is_empty()
            || self.pending_native_window_capture.is_some()
            || self.pending_resolution_query_refresh.is_some()
            || !self.session_save_tasks.is_empty()
            || !self.session_load_tasks.is_empty()
            || !self.workspace_list_import_tasks.is_empty()
            // A built-in Genes track is still being verified and decoded.
            || self.genome_tasks.iter().any(|task| task.landmark.is_some())
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
            if panel.pending_queries.reference || panel.pending_queries.tracks {
                return false;
            }
            if self.track_engine.is_some()
                && panel.track_settled_generation != Some(panel.generation_clock.current().get())
            {
                return false;
            }
            if !reference_dependency_is_settled(
                self.reference_engine.is_some()
                    && panel.viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
                panel.generation_clock.current().get(),
                self.reference_revision,
                panel.reference_slice_revision,
                panel.reference_slice.as_ref().map(|slice| slice.interval),
                panel.viewport.interval(),
                panel.reference_settled_generation,
                panel.reference_query_error_generation,
            ) {
                return false;
            }
            for track in &self.alignment_tracks {
                match track.lifecycle {
                    AlignmentTrackLifecycle::Opening => return false,
                    AlignmentTrackLifecycle::Ready => {
                        if track.scene_engine.is_none() {
                            continue;
                        }
                        let Some(projection) = track.projection(panel.id) else {
                            return false;
                        };
                        if !projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
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

    pub(super) fn automation_unsettled_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.display_dirty {
            reasons.push("display_dirty".to_owned());
        }
        if self.next_egui_repaint.is_some() {
            reasons.push("egui_repaint_pending".to_owned());
        }
        for (label, count) in [
            ("exports", self.export_tasks.len()),
            ("session_saves", self.session_save_tasks.len()),
            ("session_loads", self.session_load_tasks.len()),
            ("list_imports", self.workspace_list_import_tasks.len()),
        ] {
            if count > 0 {
                reasons.push(format!("{label}={count}"));
            }
        }
        if self.pending_native_window_capture.is_some() {
            reasons.push("native_capture_pending".to_owned());
        }
        if self.pending_resolution_query_refresh.is_some() {
            reasons.push("resolution_refresh_pending".to_owned());
        }
        if self.reference_engine.is_some() && self.reference_dictionary.is_none() {
            reasons.push("reference_opening".to_owned());
        }
        for panel in self.panels.iter() {
            if panel.navigation_presentation.is_unsettled() {
                reasons.push(format!(
                    "panel{}_presentation={:?}",
                    panel.id.get(),
                    panel.navigation_presentation.phase
                ));
            }
            if panel.navigation_query_schedule.pending.is_some() {
                reasons.push(format!("panel{}_navigation_query", panel.id.get()));
            }
            if panel.pending_queries.alignment {
                reasons.push(format!("panel{}_alignments", panel.id.get()));
            }
            if panel.pending_queries.reference {
                reasons.push(format!("panel{}_reference", panel.id.get()));
            }
            if panel.pending_queries.tracks {
                reasons.push(format!("panel{}_tracks", panel.id.get()));
            }
            for track in &self.alignment_tracks {
                if track.scene_engine.is_none() {
                    continue;
                }
                let Some(projection) = track.projection(panel.id) else {
                    reasons.push(format!(
                        "panel{}_track{}_projection",
                        panel.id.get(),
                        track.config.id.get()
                    ));
                    continue;
                };
                if !projection
                    .navigation
                    .is_current_settled(projection.clock.current().get())
                {
                    let diagnostic = projection.navigation.diagnostic();
                    reasons.push(format!(
                        "panel{}_track{}_scene_phase={:?}_owner={:?}",
                        panel.id.get(),
                        track.config.id.get(),
                        diagnostic.phase,
                        diagnostic.owner
                    ));
                }
            }
        }
        if reasons.is_empty() {
            reasons.push("unclassified".to_owned());
        }
        reasons
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
        if runner.cancel_if_requested() {
            self.detach_automation_file_tasks();
            self.report_error(
                "Local-control batch cancelled: its request timed out before it completed",
            );
            self.mark_display_dirty("explicit display invalidation");
            return;
        }
        let Some(runner) = self.automation.as_mut() else {
            return;
        };
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
                    crate::app::shell_state::report_error_to(
                        &mut self.shell.notifications,
                        &mut self.data_status,
                        format!("Could not write automation report: {error}"),
                    );
                }
                Err(_) => {
                    crate::app::shell_state::report_error_to(
                        &mut self.shell.notifications,
                        &mut self.data_status,
                        "Automation-report worker failed",
                    );
                }
            }
            runner.finished = true;
            self.automation_exit_requested = exit;
            self.mark_display_dirty("explicit display invalidation");
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
                    let reasons = self.automation_unsettled_reasons().join(", ");
                    self.finish_automation_command(
                        command_index,
                        Err((
                            AutomationErrorCode::Timeout,
                            format!(
                                "Workspace did not settle before the declared timeout: {reasons}"
                            ),
                        )),
                    );
                }
                return;
            }
            Some(AutomationWait::File {
                command_index,
                deadline,
            }) => {
                if now >= deadline {
                    // The worker may still finish later; its result must not be attributed to
                    // a later command or applied after the failure was reported.
                    self.detach_automation_file_tasks();
                    self.finish_automation_command(
                        command_index,
                        Err((
                            AutomationErrorCode::Timeout,
                            "The file operation did not complete before its deadline; a late result will be ignored"
                                .to_owned(),
                        )),
                    );
                }
                return;
            }
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
            Some(AutomationWait::TrackReply {
                command_index,
                deadline,
                pending,
            }) => {
                self.poll_automation_track_reply(command_index, deadline, &pending, now);
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

    /// Disowns every file worker started by the automation batch, so a result that arrives
    /// after the batch gave up on it is neither reported nor (for a project open) applied.
    pub(crate) fn detach_automation_file_tasks(&mut self) {
        for task in &mut self.session_save_tasks {
            task.automation_command = None;
        }
        for task in &mut self.export_tasks {
            task.automation_command = None;
        }
        for task in &mut self.session_load_tasks {
            if task.automation_command.take().is_some() {
                task.deadline = None;
                self.document_state.cancel_load(task.ticket);
            }
        }
        self.pending_native_window_capture = None;
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
        self.mark_display_dirty("explicit display invalidation");
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
                | AutomationWait::File { command_index, .. }
                | AutomationWait::Reference { command_index, .. }
                | AutomationWait::AlignmentTrack { command_index, .. }
                | AutomationWait::AuxiliaryTrack { command_index, .. }
                | AutomationWait::ExactBaseSelection { command_index, .. }
                | AutomationWait::TrackReply { command_index, .. } => *command_index,
            });
            tracing::warn!(
                command_index,
                ?waiting_index,
                "ignored a mismatched automation completion"
            );
            return;
        }
        self.mark_display_dirty("explicit display invalidation");
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
            self.mark_display_dirty("explicit display invalidation");
            return;
        }
        let json = match report.to_json() {
            Ok(json) => json,
            Err(error) => {
                crate::app::shell_state::report_error_to(
                    &mut self.shell.notifications,
                    &mut self.data_status,
                    format!("Could not serialize automation report: {error}"),
                );
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
                crate::app::shell_state::report_error_to(
                    &mut self.shell.notifications,
                    &mut self.data_status,
                    format!("Could not start automation-report writer: {error}"),
                );
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
            | AutomationWait::File { command_index, .. }
            | AutomationWait::Reference { command_index, .. }
            | AutomationWait::AlignmentTrack { command_index, .. }
            | AutomationWait::AuxiliaryTrack { command_index, .. }
            | AutomationWait::ExactBaseSelection { command_index, .. }
            | AutomationWait::TrackReply { command_index, .. } => *command_index,
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
}
