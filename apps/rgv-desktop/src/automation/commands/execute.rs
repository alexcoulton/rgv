use crate::app::tasks::SessionLoadKind;
use crate::automation::commands::AUTOMATION_FILE_WAIT_TIMEOUT;
use crate::automation::{
    AutomationAuxiliaryOpenState, AutomationExecution, AutomationWait, automation_failure,
    parse_automation_sam_tag,
};
use crate::sources::{inferred_automation_track_kind, source_path, track_source};
use crate::{AUTOMATION_SOURCE_OPEN_TIMEOUT, Viewer};
use rgv_automation::{
    AutomationCommand, AutomationErrorCode, AutomationExportKind, AutomationSelection,
    AutomationSelectionGranularity, AutomationUnsavedChangesPolicy, AutomationViewportOperation,
};
use rgv_core::{SessionDocument, SessionLocus, TrackKind, TrackSource};
use rgv_layout::{AlignmentGroupMode, AlignmentSortMode};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn execute_automation_command(
        &mut self,
        command_index: usize,
        command: AutomationCommand,
        now: Instant,
    ) -> AutomationExecution {
        self.observe_document_changes();
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
                let trace = self.diagnostics.latency_snapshot();
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
            AutomationCommand::DescribeInteractionRecording => {
                let status = self.interaction_recording_status();
                match serde_json::to_value(status) {
                    Ok(data) => AutomationExecution::Complete {
                        message: "Described the interaction recording".to_owned(),
                        data,
                    },
                    Err(error) => automation_failure(AutomationErrorCode::Internal, error),
                }
            }
            AutomationCommand::StartInteractionRecording {
                path,
                fixture,
                record_text,
            } => {
                match self.start_interaction_recording(PathBuf::from(path), fixture, record_text) {
                    Ok(status) => match serde_json::to_value(status) {
                        Ok(data) => AutomationExecution::Complete {
                            message: "Started the interaction recording".to_owned(),
                            data,
                        },
                        Err(error) => automation_failure(AutomationErrorCode::Internal, error),
                    },
                    Err(error) => automation_failure(AutomationErrorCode::InvalidState, error),
                }
            }
            AutomationCommand::StopInteractionRecording => {
                match self.stop_interaction_recording() {
                    Ok(status) => match serde_json::to_value(status) {
                        Ok(data) => AutomationExecution::Complete {
                            message: "Stopped and finalised the interaction recording".to_owned(),
                            data,
                        },
                        Err(error) => automation_failure(AutomationErrorCode::Internal, error),
                    },
                    Err(error) => automation_failure(AutomationErrorCode::InvalidState, error),
                }
            }
            AutomationCommand::BeginNativeObservation {
                run_id,
                step_id,
                expected_target,
            } => {
                match self
                    .diagnostics
                    .begin_native_observation(run_id, step_id, expected_target)
                {
                    Ok(summary) => match serde_json::to_value(summary) {
                        Ok(data) => AutomationExecution::Complete {
                            message: "Opened the native-input observation scope".to_owned(),
                            data,
                        },
                        Err(error) => automation_failure(AutomationErrorCode::Internal, error),
                    },
                    Err(error) => automation_failure(AutomationErrorCode::InvalidState, error),
                }
            }
            AutomationCommand::EndNativeObservation => {
                match self.diagnostics.end_native_observation() {
                    Ok(summary) => match serde_json::to_value(summary) {
                        Ok(data) => AutomationExecution::Complete {
                            message: "Closed the native-input observation scope".to_owned(),
                            data,
                        },
                        Err(error) => automation_failure(AutomationErrorCode::Internal, error),
                    },
                    Err(error) => automation_failure(AutomationErrorCode::InvalidState, error),
                }
            }
            AutomationCommand::CheckpointInteractionRecording { label } => {
                let snapshot = self.automation_workspace_snapshot();
                match self.diagnostics.checkpoint_recording(label, snapshot) {
                    Ok(status) => match serde_json::to_value(status) {
                        Ok(data) => AutomationExecution::Complete {
                            message: "Recorded an interaction checkpoint".to_owned(),
                            data,
                        },
                        Err(error) => automation_failure(AutomationErrorCode::Internal, error),
                    },
                    Err(error) => automation_failure(AutomationErrorCode::InvalidState, error),
                }
            }
            AutomationCommand::StartDiagnosticTrace => {
                self.start_manual_diagnostic_trace("interaction stress controller", false);
                if self.diagnostics.recording() {
                    AutomationExecution::Complete {
                        message: "Started the diagnostic trace".to_owned(),
                        data: serde_json::json!({"recording": true}),
                    }
                } else {
                    // Only reachable in a build without the `diagnostics` feature.
                    automation_failure(
                        AutomationErrorCode::InvalidState,
                        "The diagnostic trace is not available in this build",
                    )
                }
            }
            AutomationCommand::StopDiagnosticTrace => {
                self.stop_and_save_manual_diagnostic_trace();
                let snapshot = self.manual_diagnostic_trace_snapshot();
                AutomationExecution::Complete {
                    message: "Stopped and saved the diagnostic trace".to_owned(),
                    data: serde_json::json!({
                        "recording": false,
                        "event_count": snapshot.event_count,
                        "dropped_event_count": snapshot.dropped_event_count,
                        "saved_path": snapshot.saved_path,
                        "failure": snapshot.failure,
                    }),
                }
            }
            AutomationCommand::ClearSelection => {
                let previous = self.automation_selection_snapshot();
                self.panels.active_mut().clear_selection();
                self.bump_workspace_revision();
                self.rebuild_genomic_display(self.panels.active().id);
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
                timeout_millis,
            } => {
                if !confirm_replace {
                    return automation_failure(
                        AutomationErrorCode::WorkspaceReplacementNotConfirmed,
                        "workspace replacement was not confirmed",
                    );
                }
                let path = PathBuf::from(path);
                let dirty = self.document_state.is_dirty();
                let mut save_destination = None;
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
                            save_destination = Some(current_path);
                        }
                        AutomationUnsavedChangesPolicy::Discard => {}
                    }
                }
                match self.try_start_session_load(
                    path,
                    SessionLoadKind::Native,
                    Some(command_index),
                    Some(now + Duration::from_millis(timeout_millis)),
                    save_destination,
                ) {
                    // The load task enforces the declared timeout itself; this backstop only
                    // covers a worker that never reports.
                    Ok(()) => AutomationExecution::Wait(AutomationWait::File {
                        command_index,
                        deadline: now
                            + Duration::from_millis(timeout_millis)
                            + AUTOMATION_FILE_WAIT_TIMEOUT,
                    }),
                    Err(error) => automation_failure(AutomationErrorCode::Io, error),
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
                match self
                    .apply_command_translation(&AutomationCommand::RemoveTrack { track_id })
                    .map(|outcome| outcome.removed_tracks)
                {
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
            AutomationCommand::Goto { loci } => match self
                .apply_command_translation(&AutomationCommand::Goto { loci: loci.clone() })
            {
                Ok(_) => {
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
            AutomationCommand::AddPanel { locus } => match self
                .apply_command_translation(&AutomationCommand::AddPanel {
                    locus: locus.clone(),
                })
                .and_then(|outcome| {
                    outcome
                        .panel_id
                        .ok_or_else(|| anyhow::anyhow!("the panel was not created"))
                }) {
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
                if self
                    .apply_command_translation(&AutomationCommand::ClosePanel { panel_id })
                    .is_ok_and(|outcome| outcome.changed)
                {
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
                anchor_position,
            } => {
                let requested_mode = tag
                    .as_deref()
                    .and_then(|tag| parse_automation_sam_tag(tag).ok())
                    .map_or(mode, AlignmentGroupMode::Tag);
                let before = self.automation_alignment_group_state(&track_ids);
                match self.apply_automation_group(
                    &track_ids,
                    mode,
                    direction,
                    tag.as_deref(),
                    anchor_position,
                ) {
                    Ok(()) => {
                        let effective = self.automation_alignment_group_state(&track_ids);
                        let changed = before != effective;
                        self.bump_workspace_revision();
                        AutomationExecution::Complete {
                            message: format!("Grouped {} alignment tracks", track_ids.len()),
                            data: serde_json::json!({
                                "requested": {"mode": requested_mode, "direction": direction, "anchor": anchor_position},
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
                anchor_position,
            } => {
                let requested_mode = tag
                    .as_deref()
                    .and_then(|tag| parse_automation_sam_tag(tag).ok())
                    .map_or(mode, AlignmentSortMode::Tag);
                let before = self.automation_alignment_sort_state(&track_ids);
                match self.apply_automation_sort(
                    &track_ids,
                    mode,
                    direction,
                    tag.as_deref(),
                    anchor_position,
                ) {
                    Ok(()) => {
                        let effective = self.automation_alignment_sort_state(&track_ids);
                        let changed = before != effective;
                        self.bump_workspace_revision();
                        AutomationExecution::Complete {
                            message: format!("Sorted {} alignment tracks", track_ids.len()),
                            data: serde_json::json!({
                                "requested": {"mode": requested_mode, "direction": direction, "anchor": anchor_position},
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
            AutomationCommand::QueryFeatures {
                contig,
                start,
                end,
                track_ids,
                offset,
                limit,
            } => self.start_automation_feature_query(
                command_index,
                &contig,
                start,
                end,
                track_ids.as_deref(),
                offset,
                limit,
                now,
            ),
            AutomationCommand::GotoGene {
                query,
                panel_id,
                flank_bases,
            } => {
                self.start_automation_gene_search(command_index, &query, panel_id, flank_bases, now)
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
                scale,
            } => match self.start_automation_snapshot(
                command_index,
                PathBuf::from(path),
                scope,
                format,
                scale,
            ) {
                Ok(()) => AutomationExecution::Wait(AutomationWait::File {
                    command_index,
                    deadline: now + AUTOMATION_FILE_WAIT_TIMEOUT,
                }),
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
                    Ok(()) => AutomationExecution::Wait(AutomationWait::File {
                        command_index,
                        deadline: now + AUTOMATION_FILE_WAIT_TIMEOUT,
                    }),
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
                    Ok(()) => AutomationExecution::Wait(AutomationWait::File {
                        command_index,
                        deadline: now + AUTOMATION_FILE_WAIT_TIMEOUT,
                    }),
                    Err(error) => automation_failure(AutomationErrorCode::Io, error),
                }
            }
            AutomationCommand::Exit => AutomationExecution::Complete {
                message: "Exit requested after the automation report is durable".to_owned(),
                data: serde_json::json!({"exit": true}),
            },
        }
    }
}
