use super::super::*;

pub(crate) struct ExportTask {
    pub(crate) path: PathBuf,
    pub(crate) automation_command: Option<usize>,
    pub(crate) automation_data: Option<serde_json::Value>,
    pub(crate) worker: JoinHandle<std::io::Result<()>>,
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

impl Viewer {
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
