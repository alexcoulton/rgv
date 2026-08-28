use super::super::*;

pub(crate) struct ExportTask {
    pub(crate) path: PathBuf,
    pub(crate) automation_command: Option<usize>,
    pub(crate) automation_data: Option<serde_json::Value>,
    pub(crate) worker: JoinHandle<std::io::Result<()>>,
}

pub(crate) struct NativeWindowCaptureTask {
    pub(crate) path: PathBuf,
    pub(crate) automation_command: usize,
    pub(crate) automation_data: serde_json::Value,
}

pub(crate) struct SessionSaveTask {
    pub(crate) path: PathBuf,
    pub(crate) kind: SessionSaveKind,
    pub(crate) automation_command: Option<usize>,
    pub(crate) worker: JoinHandle<std::result::Result<(), String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionSaveKind {
    Manual,
    Autosave,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionLoadKind {
    Native,
    IgvImport,
    Recovery,
}

pub(crate) struct SessionLoadTask {
    pub(crate) path: PathBuf,
    pub(crate) kind: SessionLoadKind,
    pub(crate) worker: JoinHandle<std::result::Result<LoadedSession, String>>,
}

pub(crate) struct LoadedSession {
    pub(crate) document: SessionDocument,
    pub(crate) igv_report: Option<IgvImportReport>,
}

pub(crate) struct WorkspaceListImportTask {
    pub(crate) path: PathBuf,
    pub(crate) worker: JoinHandle<std::result::Result<WorkspaceListImport, String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AlignmentRowLayoutTaskKey {
    pub(crate) query_identity: usize,
    pub(crate) layout_query_identity: usize,
    pub(crate) group_mode: AlignmentGroupMode,
    pub(crate) group_order: AlignmentOrderDirection,
    pub(crate) sort_mode: AlignmentSortMode,
    pub(crate) sort_order: AlignmentOrderDirection,
    pub(crate) sort_position: Option<u64>,
    pub(crate) promoted_read_name_hash: Option<u64>,
    pub(crate) max_rows: usize,
    pub(crate) view_as_pairs: bool,
    pub(crate) pair_length_bounds: TemplateLengthBounds,
    pub(crate) stacking_mode: ReadStackingMode,
    pub(crate) repack: bool,
}

pub(crate) struct AlignmentRowLayoutTask {
    pub(crate) panel_id: PanelId,
    pub(crate) track_id: TrackId,
    pub(crate) request_revision: u64,
    pub(crate) key: AlignmentRowLayoutTaskKey,
}

pub(crate) struct AlignmentRowLayoutTaskResult {
    pub(crate) rows: CachedAlignmentRows,
    pub(crate) history: AlignmentRowHistory,
}

pub(crate) struct AlignmentRowLayoutWork {
    panel_id: PanelId,
    track_id: TrackId,
    request_revision: u64,
    key: AlignmentRowLayoutTaskKey,
    query: Arc<AlignmentQuery>,
    layout_query: Arc<AlignmentQuery>,
    options: AlignmentLayoutOptions,
    stacking_mode: ReadStackingMode,
    repack: bool,
    refused: bool,
    history: AlignmentRowHistory,
    cancellation_clock: GenerationClock,
    cancellation_generation: ViewportGeneration,
}

pub(crate) struct CompletedAlignmentRowLayoutWork {
    panel_id: PanelId,
    track_id: TrackId,
    request_revision: u64,
    key: AlignmentRowLayoutTaskKey,
    result: Option<AlignmentRowLayoutTaskResult>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AlignmentRowLayoutSubmit {
    Submitted,
    QueueFull,
    Disconnected,
}

pub(crate) fn process_alignment_row_layout_work(
    work: AlignmentRowLayoutWork,
    _scheduler_clock: &GenerationClock,
    _scheduler_generation: ViewportGeneration,
) -> Option<CompletedAlignmentRowLayoutWork> {
    let AlignmentRowLayoutWork {
        panel_id,
        track_id,
        request_revision,
        key,
        query,
        layout_query,
        options,
        stacking_mode,
        repack,
        refused,
        history,
        cancellation_clock,
        cancellation_generation,
    } = work;
    let result = if refused {
        if !cancellation_clock.is_current(cancellation_generation) {
            None
        } else {
            let layout = assign_alignment_rows(&query, options, None);
            cancellation_clock
                .is_current(cancellation_generation)
                .then_some((layout, AlignmentRowReconciliationStats::default(), history))
        }
    } else {
        reconcile_alignment_rows_with_horizon_cancellable(
            &query,
            &layout_query,
            options,
            history,
            stacking_mode,
            repack,
            || !cancellation_clock.is_current(cancellation_generation),
        )
    }
    .map(
        |(layout, reconciliation, history)| AlignmentRowLayoutTaskResult {
            rows: CachedAlignmentRows {
                query,
                layout_query,
                layout: Arc::new(layout),
                group_mode: options.group_mode,
                group_order: options.group_order,
                sort_mode: options.sort_mode,
                sort_order: options.sort_order,
                sort_position: options.sort_position,
                promoted_read_name_hash: options.promoted_read_name_hash,
                max_rows: options.max_rows,
                view_as_pairs: options.view_as_pairs,
                pair_length_bounds: options.pair_length_bounds,
                stacking_mode,
                reconciliation,
            },
            history,
        },
    );
    Some(CompletedAlignmentRowLayoutWork {
        panel_id,
        track_id,
        request_revision,
        key,
        result,
    })
}

impl Viewer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start_alignment_row_layout_task(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
        request_revision: u64,
        key: AlignmentRowLayoutTaskKey,
        query: Arc<AlignmentQuery>,
        layout_query: Arc<AlignmentQuery>,
        options: AlignmentLayoutOptions,
        stacking_mode: ReadStackingMode,
        repack: bool,
        refused: bool,
        history: AlignmentRowHistory,
        cancellation_clock: GenerationClock,
        cancellation_generation: ViewportGeneration,
    ) -> AlignmentRowLayoutSubmit {
        if self
            .alignment_row_layout_tasks
            .iter()
            .any(|task| task.panel_id == panel_id && task.track_id == track_id && task.key == key)
        {
            return AlignmentRowLayoutSubmit::Submitted;
        }
        let work = AlignmentRowLayoutWork {
            panel_id,
            track_id,
            request_revision,
            key,
            query,
            layout_query,
            options,
            stacking_mode,
            repack,
            refused,
            history,
            cancellation_clock,
            cancellation_generation,
        };
        match self.alignment_row_layout_scheduler.try_submit(WorkItem {
            generation: self.alignment_row_layout_scheduler_clock.current(),
            payload: work,
        }) {
            Ok(()) => {
                self.alignment_row_layout_tasks
                    .push(AlignmentRowLayoutTask {
                        panel_id,
                        track_id,
                        request_revision,
                        key,
                    });
                AlignmentRowLayoutSubmit::Submitted
            }
            Err(SchedulerError::QueueFull(_)) => AlignmentRowLayoutSubmit::QueueFull,
            Err(SchedulerError::Disconnected(_)) => AlignmentRowLayoutSubmit::Disconnected,
        }
    }

    pub(crate) fn poll_alignment_row_layout_tasks(&mut self) {
        let mut committed = false;
        let mut processed = false;
        loop {
            let completed = match self.alignment_row_layout_scheduler.try_receive() {
                Ok(Some(completed)) => completed.output,
                Ok(None) => break,
                Err(_) => {
                    self.data_status = "Read-layout worker pool disconnected".to_owned();
                    self.display_dirty = true;
                    break;
                }
            };
            processed = true;
            if let Some(index) = self.alignment_row_layout_tasks.iter().position(|task| {
                task.panel_id == completed.panel_id
                    && task.track_id == completed.track_id
                    && task.request_revision == completed.request_revision
                    && task.key == completed.key
            }) {
                self.alignment_row_layout_tasks.remove(index);
            }
            let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == completed.track_id)
            else {
                continue;
            };
            let projection = track.projection_mut(completed.panel_id);
            let result_is_current = projection.row_layout_request_revision
                == completed.request_revision
                && completed.result.as_ref().is_some_and(|result| {
                    projection
                        .current_query
                        .as_ref()
                        .is_some_and(|query| Arc::ptr_eq(query, &result.rows.query))
                        && projection
                            .layout_query
                            .as_ref()
                            .or(projection.current_query.as_ref())
                            .is_some_and(|query| Arc::ptr_eq(query, &result.rows.layout_query))
                });
            match completed.result {
                Some(result) if result_is_current => {
                    projection.row_history = result.history;
                    projection.alignment_rows = Some(result.rows);
                    projection.cached_layout = None;
                    if completed.key.repack {
                        projection.pending_row_repack = false;
                    }
                    committed = true;
                }
                Some(_) => {
                    self.navigation_stale_result_rejections =
                        self.navigation_stale_result_rejections.saturating_add(1);
                }
                None => {
                    self.alignment_row_layout_cancellations =
                        self.alignment_row_layout_cancellations.saturating_add(1);
                }
            }
        }
        if committed || processed {
            self.layout_cache.clear();
            self.display_dirty = true;
            self.window.request_redraw();
        }
    }

    pub(crate) fn finish_alignment_row_layout_tasks(&mut self) {
        for track in &self.alignment_tracks {
            for projection in track.projections.values() {
                projection.row_layout_clock.advance();
            }
        }
        self.alignment_row_layout_scheduler_clock.advance();
        self.alignment_row_layout_tasks.clear();
    }

    pub(crate) fn start_export(&mut self, path: PathBuf, contents: String) {
        self.start_export_job(path, move |path| std::fs::write(path, contents));
    }

    pub(crate) fn start_session_save(&mut self, path: PathBuf) {
        if self.session_save_tasks.iter().any(|task| task.path == path) {
            self.data_status = format!("A save to {} is already in progress", path.display());
            return;
        }
        let document = self.session_document();
        let display_path = path.display().to_string();
        match std::thread::Builder::new()
            .name("rgv-session-save".to_owned())
            .spawn({
                let path = path.clone();
                move || document.write(path).map_err(|error| error.to_string())
            }) {
            Ok(worker) => {
                self.session_save_tasks.push(SessionSaveTask {
                    path,
                    kind: SessionSaveKind::Manual,
                    automation_command: None,
                    worker,
                });
                self.data_status = format!("Saving project {display_path}…");
            }
            Err(error) => self.data_status = format!("Could not start project save: {error}"),
        }
    }

    pub(crate) fn schedule_next_autosave(&mut self) {
        self.next_autosave =
            Instant::now() + Duration::from_secs(self.settings.autosave_interval_seconds);
    }

    pub(crate) fn maybe_start_autosave(&mut self, now: Instant) {
        if !self.settings.autosave_enabled || now < self.next_autosave {
            return;
        }
        self.next_autosave = now + Duration::from_secs(self.settings.autosave_interval_seconds);
        if self
            .session_save_tasks
            .iter()
            .any(|task| task.kind == SessionSaveKind::Autosave)
        {
            return;
        }
        let document = self.session_document();
        let (path, prefix) = autosave_snapshot_path(
            &self.settings_path,
            self.current_session_path.as_deref(),
            SystemTime::now(),
        );
        let directory = autosave_directory(&self.settings_path);
        let retention = self.settings.autosave_retention;
        match std::thread::Builder::new()
            .name("rgv-autosave".to_owned())
            .spawn({
                let path = path.clone();
                move || {
                    document.write(&path).map_err(|error| error.to_string())?;
                    prune_autosaves(&directory, &prefix, retention)
                        .map_err(|error| error.to_string())
                }
            }) {
            Ok(worker) => self.session_save_tasks.push(SessionSaveTask {
                path,
                kind: SessionSaveKind::Autosave,
                automation_command: None,
                worker,
            }),
            Err(error) => {
                self.data_status = format!("Could not start recovery autosave: {error}");
                self.display_dirty = true;
            }
        }
    }

    pub(crate) fn poll_session_save_tasks(&mut self) {
        let mut index = 0;
        while index < self.session_save_tasks.len() {
            if !self.session_save_tasks[index].worker.is_finished() {
                index += 1;
                continue;
            }
            let task = self.session_save_tasks.remove(index);
            let path = task.path;
            let outcome = task.worker.join();
            let automation_command = task.automation_command;
            let (status, automation_outcome) = match (task.kind, outcome) {
                (SessionSaveKind::Manual, Ok(Ok(()))) => {
                    self.last_saved_workspace_revision = self.workspace_revision;
                    self.current_session_path = Some(path.clone());
                    self.settings.remember_session(path.display().to_string());
                    self.persist_settings();
                    (
                        format!("Saved project to {}", path.display()),
                        automation_command.map(|_| {
                            let bytes = std::fs::read(&path).unwrap_or_default();
                            let mut digest = String::with_capacity(64);
                            for byte in Sha256::digest(&bytes) {
                                let _ = write!(digest, "{byte:02x}");
                            }
                            Ok((
                                format!("Saved project to {}", path.display()),
                                serde_json::json!({
                                    "path": path.display().to_string(),
                                    "bytes": bytes.len(),
                                    "sha256": digest,
                                }),
                            ))
                        }),
                    )
                }
                (SessionSaveKind::Manual, Ok(Err(error))) => (
                    format!("Could not save project {}: {error}", path.display()),
                    automation_command.map(|_| Err((AutomationErrorCode::Io, error))),
                ),
                (SessionSaveKind::Manual, Err(_)) => {
                    let message = format!(
                        "Project-save worker failed while writing {}",
                        path.display()
                    );
                    (
                        message.clone(),
                        automation_command.map(|_| Err((AutomationErrorCode::Internal, message))),
                    )
                }
                (SessionSaveKind::Autosave, Ok(Ok(()))) => (
                    format!("Recovery autosave updated at {}", path.display()),
                    None,
                ),
                (SessionSaveKind::Autosave, Ok(Err(error))) => {
                    (format!("Could not update recovery autosave: {error}"), None)
                }
                (SessionSaveKind::Autosave, Err(_)) => {
                    ("Recovery-autosave worker failed".to_owned(), None)
                }
            };
            self.data_status = status;
            if let (Some(command_index), Some(outcome)) = (automation_command, automation_outcome) {
                self.finish_automation_command(command_index, outcome);
            }
            self.display_dirty = true;
        }
    }

    pub(crate) fn finish_session_save_tasks(&mut self) {
        for task in self.session_save_tasks.drain(..) {
            let path = task.path.display().to_string();
            match task.worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, %path, "project save failed during shutdown");
                }
                Err(_) => tracing::warn!(%path, "project-save worker panicked during shutdown"),
            }
        }
    }

    pub(crate) fn start_session_load(&mut self, path: PathBuf, kind: SessionLoadKind) {
        if self.session_load_tasks.iter().any(|task| task.path == path) {
            self.data_status = format!("{} is already being opened", path.display());
            return;
        }
        let display_path = path.display().to_string();
        match std::thread::Builder::new()
            .name("rgv-session-load".to_owned())
            .spawn({
                let path = path.clone();
                move || match kind {
                    SessionLoadKind::Native | SessionLoadKind::Recovery => {
                        SessionDocument::read(path)
                            .map(|document| LoadedSession {
                                document,
                                igv_report: None,
                            })
                            .map_err(|error| error.to_string())
                    }
                    SessionLoadKind::IgvImport => std::fs::read_to_string(&path)
                        .map_err(|error| error.to_string())
                        .and_then(|xml| {
                            SessionDocument::import_igv_with_report(&xml)
                                .map_err(|error| error.to_string())
                        })
                        .and_then(|imported| {
                            let mut document = imported.document;
                            let parent = path
                                .parent()
                                .filter(|parent| !parent.as_os_str().is_empty())
                                .unwrap_or_else(|| Path::new("."));
                            let parent =
                                std::path::absolute(parent).map_err(|error| error.to_string())?;
                            document.resolve_project_paths(parent);
                            Ok(LoadedSession {
                                document,
                                igv_report: Some(imported.report),
                            })
                        }),
                }
            }) {
            Ok(worker) => {
                self.session_load_tasks
                    .push(SessionLoadTask { path, kind, worker });
                self.data_status = format!("Opening {display_path}…");
            }
            Err(error) => self.data_status = format!("Could not start project open: {error}"),
        }
    }

    pub(crate) fn poll_session_load_tasks(&mut self) {
        let mut index = 0;
        while index < self.session_load_tasks.len() {
            if !self.session_load_tasks[index].worker.is_finished() {
                index += 1;
                continue;
            }
            let task = self.session_load_tasks.remove(index);
            let path = task.path;
            match task.worker.join() {
                Ok(Ok(loaded)) => {
                    let LoadedSession {
                        document,
                        igv_report,
                    } = loaded;
                    let unresolved = document.unresolved_local_sources();
                    match self.apply_session(&document) {
                        Ok(()) => {
                            self.igv_import_report = igv_report;
                            match task.kind {
                                SessionLoadKind::Native => {
                                    self.current_session_path = Some(path.clone());
                                    self.settings.remember_session(path.display().to_string());
                                    self.persist_settings();
                                    self.data_status = opened_project_status(&path, &unresolved);
                                }
                                SessionLoadKind::IgvImport => {
                                    self.current_session_path = None;
                                    self.settings.remember_session(path.display().to_string());
                                    self.persist_settings();
                                    self.data_status = if unresolved.is_empty() {
                                        format!(
                                            "Imported IGV session {} · {}",
                                            path.display(),
                                            self.igv_import_report
                                                .as_ref()
                                                .map_or_else(String::new, IgvImportReport::summary)
                                        )
                                    } else {
                                        opened_project_status(&path, &unresolved)
                                    };
                                    self.open_panel = Some(UiPanel::IgvImportReport);
                                }
                                SessionLoadKind::Recovery => {
                                    self.current_session_path = None;
                                    self.recovery_open = false;
                                    self.data_status = format!(
                                        "Recovered autosave {} · use Save project as… to keep it",
                                        path.display()
                                    );
                                }
                            }
                            self.schedule_next_autosave();
                        }
                        Err(error) => {
                            self.data_status =
                                format!("Could not apply project {}: {error}", path.display());
                        }
                    }
                }
                Ok(Err(error)) => {
                    self.data_status = format!("Could not open {}: {error}", path.display());
                }
                Err(_) => {
                    self.data_status = format!(
                        "Project-load worker failed while reading {}",
                        path.display()
                    );
                }
            }
            self.display_dirty = true;
        }
    }

    pub(crate) fn finish_session_load_tasks(&mut self) {
        for task in self.session_load_tasks.drain(..) {
            if task.worker.join().is_err() {
                tracing::warn!(path = %task.path.display(), "project-load worker panicked during shutdown");
            }
        }
    }

    pub(crate) fn start_export_job(
        &mut self,
        path: PathBuf,
        job: impl FnOnce(PathBuf) -> std::io::Result<()> + Send + 'static,
    ) {
        let display_path = path.display().to_string();
        match std::thread::Builder::new()
            .name("rgv-export".to_owned())
            .spawn({
                let path = path.clone();
                move || job(path)
            }) {
            Ok(worker) => {
                self.export_tasks.push(ExportTask {
                    path,
                    automation_command: None,
                    automation_data: None,
                    worker,
                });
                self.data_status = format!("Exporting {display_path}…");
            }
            Err(error) => self.data_status = format!("Could not start export: {error}"),
        }
    }

    pub(crate) fn poll_export_tasks(&mut self) {
        let mut index = 0;
        while index < self.export_tasks.len() {
            if !self.export_tasks[index].worker.is_finished() {
                index += 1;
                continue;
            }
            let task = self.export_tasks.remove(index);
            let path = task.path.display().to_string();
            let automation_command = task.automation_command;
            let automation_data = task.automation_data;
            let (status, automation_outcome) = match task.worker.join() {
                Ok(Ok(())) => (
                    format!("Exported {path}"),
                    automation_command.map(|_| {
                        let mut data = automation_data.unwrap_or_else(|| serde_json::json!({}));
                        if let Some(object) = data.as_object_mut() {
                            object.insert("path".to_owned(), path.clone().into());
                        }
                        Ok((format!("Exported {path}"), data))
                    }),
                ),
                Ok(Err(error)) => (
                    format!("Could not export {path}: {error}"),
                    automation_command.map(|_| Err((AutomationErrorCode::Io, error.to_string()))),
                ),
                Err(_) => {
                    let message = format!("Export worker failed while writing {path}");
                    (
                        message.clone(),
                        automation_command.map(|_| Err((AutomationErrorCode::Internal, message))),
                    )
                }
            };
            self.data_status = status;
            if let (Some(command_index), Some(outcome)) = (automation_command, automation_outcome) {
                self.finish_automation_command(command_index, outcome);
            }
            self.display_dirty = true;
        }
    }

    pub(crate) fn finish_export_tasks(&mut self) {
        for task in self.export_tasks.drain(..) {
            let path = task.path.display().to_string();
            match task.worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(%error, %path, "export failed during shutdown"),
                Err(_) => tracing::warn!(%path, "export worker panicked during shutdown"),
            }
        }
    }

    pub(crate) fn start_workspace_list_import(&mut self, path: PathBuf, kind: WorkspaceListKind) {
        let display_path = path.display().to_string();
        match std::thread::Builder::new()
            .name("rgv-list-import".to_owned())
            .spawn({
                let path = path.clone();
                move || import_workspace_list(&path, kind)
            }) {
            Ok(worker) => {
                self.workspace_list_import_tasks
                    .push(WorkspaceListImportTask { path, worker });
                self.data_status = format!("Importing {display_path}…");
            }
            Err(error) => self.data_status = format!("Could not start import: {error}"),
        }
    }

    pub(crate) fn poll_workspace_list_import_tasks(&mut self) {
        let mut index = 0;
        while index < self.workspace_list_import_tasks.len() {
            if !self.workspace_list_import_tasks[index].worker.is_finished() {
                index += 1;
                continue;
            }
            let task = self.workspace_list_import_tasks.remove(index);
            let path = task.path.display().to_string();
            match task.worker.join() {
                Ok(Ok(WorkspaceListImport::Regions(imported))) => {
                    let (added, omitted) = merge_imported_regions(
                        &mut self.regions_of_interest,
                        imported,
                        MAXIMUM_REGIONS_OF_INTEREST,
                    );
                    if added > 0 {
                        self.rebuild_all_panel_displays();
                    }
                    self.data_status = imported_list_status("regions", added, omitted, &path);
                }
                Ok(Ok(WorkspaceListImport::Review(imported))) => {
                    let (added, omitted) = merge_imported_review_items(
                        &mut self.review_items,
                        imported,
                        MAXIMUM_REVIEW_ITEMS,
                    );
                    self.data_status = imported_list_status("review items", added, omitted, &path);
                }
                Ok(Ok(WorkspaceListImport::SampleMetadata(imported))) => {
                    let rows = imported.rows.len();
                    let columns = imported.columns.len();
                    self.sample_metadata = Some(imported);
                    self.metadata_filter_column = 0;
                    self.metadata_filter_value = None;
                    self.metadata_selected_tracks.clear();
                    self.data_status =
                        format!("Imported {rows} sample rows and {columns} attributes from {path}");
                }
                Ok(Err(error)) => self.data_status = format!("Could not import {path}: {error}"),
                Err(_) => self.data_status = format!("Import worker failed while reading {path}"),
            }
            self.display_dirty = true;
        }
    }

    pub(crate) fn finish_workspace_list_import_tasks(&mut self) {
        for task in self.workspace_list_import_tasks.drain(..) {
            let path = task.path.display().to_string();
            match task.worker.join() {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, %path, "workspace-list import failed during shutdown");
                }
                Err(_) => tracing::warn!(%path, "workspace-list import worker panicked"),
            }
        }
    }
}
