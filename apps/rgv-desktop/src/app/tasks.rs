use super::super::*;
use super::document_state::DocumentTicket;

/// All writers share this key, including `SaveThenOpen` and recovery autosaves.
fn normalized_session_destination(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut resolved) => {
                for name in suffix.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| std::io::Error::other("invalid project destination"))?,
                );
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| std::io::Error::other("project destination has no parent"))?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn same_session_destination(left: &Path, right: &Path) -> bool {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        // Conservative serialization also on case-sensitive volumes: a false
        // positive is a temporary Busy error, never an out-of-order overwrite.
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    } else {
        left == right
    }
}

#[cfg(test)]
mod file_identity_tests {
    use super::*;

    #[test]
    fn destination_identity_covers_new_directories_and_existing_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let new = directory.path().join("autosaves/nested/project.rgv");
        let normalized = normalized_session_destination(&new).unwrap();
        assert_eq!(
            normalized,
            directory
                .path()
                .canonicalize()
                .unwrap()
                .join("autosaves/nested/project.rgv")
        );
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        assert_eq!(normalized_session_destination(&new).unwrap(), normalized);
        #[cfg(unix)]
        {
            let alias = directory.path().join("alias");
            std::os::unix::fs::symlink(new.parent().unwrap(), &alias).unwrap();
            assert_eq!(
                normalized_session_destination(&alias.join("project.rgv")).unwrap(),
                normalized
            );
        }
    }
}

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
    pub(crate) ticket: DocumentTicket,
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
    pub(crate) ticket: DocumentTicket,
    pub(crate) automation_command: Option<usize>,
    pub(crate) deadline: Option<Instant>,
    pub(crate) save_destination: Option<PathBuf>,
    pub(crate) save_completed: Arc<std::sync::atomic::AtomicBool>,
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
        if let Err(error) = self.try_start_session_save(path, None) {
            self.report_error(format!("Could not start project save: {error}"));
        }
    }

    pub(crate) fn ensure_session_destination_available(&self, path: &Path) -> Result<PathBuf> {
        let path = normalized_session_destination(path)?;
        anyhow::ensure!(
            !self
                .session_save_tasks
                .iter()
                .any(|task| same_session_destination(&task.path, &path))
                && !self.session_load_tasks.iter().any(|task| task
                    .save_destination
                    .as_ref()
                    .is_some_and(|destination| same_session_destination(destination, &path))),
            "a save to {} is already in progress",
            path.display()
        );
        Ok(path)
    }

    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn try_start_session_save(
        &mut self,
        path: PathBuf,
        automation_command: Option<usize>,
    ) -> Result<()> {
        let path = self.ensure_session_destination_available(&path)?;
        self.observe_document_changes();
        let document = self.session_document();
        let ticket = self.document_state.ticket();
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
                    automation_command,
                    ticket,
                    worker,
                });
                self.data_status = format!("Saving project {display_path}…");
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
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
        self.observe_document_changes();
        // Rewriting an unchanged document would only rotate distinct older snapshots out of the
        // retention window.
        let marker = self.document_state.autosave_marker();
        if !crate::app::shell_state::autosave_needed(
            self.document_state.is_dirty(),
            marker,
            self.document_state.shell.last_autosaved,
        ) {
            return;
        }
        let document = self.session_document();
        let ticket = self.document_state.ticket();
        let (path, prefix) = autosave_snapshot_path(
            &self.settings_path,
            self.current_session_path.as_deref(),
            &self.application_instance_id,
            SystemTime::now(),
        );
        let path = match self.ensure_session_destination_available(&path) {
            Ok(path) => path,
            Err(error) => {
                self.data_status = error.to_string();
                return;
            }
        };
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
            Ok(worker) => {
                // Recorded when the write starts; a failed write clears it so the next interval
                // retries.
                self.document_state.shell.last_autosaved = Some(marker);
                self.session_save_tasks.push(SessionSaveTask {
                    path,
                    kind: SessionSaveKind::Autosave,
                    automation_command: None,
                    ticket,
                    worker,
                });
            }
            Err(error) => {
                self.report_error(format!("Could not start recovery autosave: {error}"));
                self.mark_display_dirty("explicit display invalidation");
            }
        }
    }

    pub(crate) fn poll_session_save_tasks(&mut self) {
        if self.session_save_tasks.is_empty() {
            return;
        }
        let mut index = 0;
        while index < self.session_save_tasks.len() {
            if !self.session_save_tasks[index].worker.is_finished() {
                index += 1;
                continue;
            }
            // Compare the current persisted values before applying this result,
            // not on every redraw while a potentially slow writer is pending.
            self.observe_document_changes();
            let task = self.session_save_tasks.remove(index);
            let path = task.path;
            let outcome = task.worker.join();
            let automation_command = task.automation_command;
            let manual_save_succeeded = match (task.kind, &outcome) {
                (SessionSaveKind::Manual, Ok(Ok(()))) => Some(true),
                (SessionSaveKind::Manual, _) => Some(false),
                (SessionSaveKind::Autosave, Ok(Ok(()))) => None,
                (SessionSaveKind::Autosave, _) => {
                    self.document_state.shell.last_autosaved = None;
                    None
                }
            };
            let failed = !matches!(outcome, Ok(Ok(())));
            let (status, automation_outcome) = match (task.kind, outcome) {
                (SessionSaveKind::Manual, Ok(Ok(()))) => {
                    if self.document_state.complete_save(task.ticket) {
                        self.current_session_path = Some(path.clone());
                        self.settings.remember_session(path.display().to_string());
                        self.persist_settings();
                    }
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
            if failed {
                self.report_error(status);
            } else {
                self.data_status = status;
            }
            if let (Some(command_index), Some(outcome)) = (automation_command, automation_outcome) {
                self.finish_automation_command(command_index, outcome);
            }
            if let Some(succeeded) = manual_save_succeeded {
                // Resume an exit or project open that was waiting for this save.
                self.unsaved_gate_save_finished(succeeded);
            }
            self.mark_display_dirty("explicit display invalidation");
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
        if let Err(error) = self.try_start_session_load(path, kind, None, None, None) {
            self.report_error(format!("Could not start project open: {error}"));
        }
    }

    pub(crate) fn try_start_session_load(
        &mut self,
        path: PathBuf,
        kind: SessionLoadKind,
        automation_command: Option<usize>,
        deadline: Option<Instant>,
        save_destination: Option<PathBuf>,
    ) -> Result<()> {
        self.observe_document_changes();
        // Issuing a request supersedes old ones even if this request fails to start.
        let ticket = self.document_state.begin_load();
        anyhow::ensure!(
            self.session_load_tasks.len() < 8,
            "previous project reads are still finishing; retry shortly"
        );
        let save_destination = save_destination
            .map(|path| self.ensure_session_destination_available(&path))
            .transpose()?;
        let save_document = save_destination
            .as_ref()
            .map(|path| (path.clone(), self.session_document()));
        let save_completed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let display_path = path.display().to_string();
        match std::thread::Builder::new()
            .name("rgv-session-load".to_owned())
            .spawn({
                let path = path.clone();
                let save_completed = Arc::clone(&save_completed);
                move || {
                    if let Some((destination, document)) = save_document {
                        document
                            .write(destination)
                            .map_err(|error| error.to_string())?;
                        save_completed.store(true, std::sync::atomic::Ordering::Release);
                    }
                    match kind {
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
                                let parent = std::path::absolute(parent)
                                    .map_err(|error| error.to_string())?;
                                document.resolve_project_paths(parent);
                                let mut report = imported.report;
                                crate::workspace::genomes::resolve_imported_session_genome(
                                    &mut document,
                                    &mut report,
                                );
                                Ok(LoadedSession {
                                    document,
                                    igv_report: Some(report),
                                })
                            }),
                    }
                }
            }) {
            Ok(worker) => {
                self.session_load_tasks.push(SessionLoadTask {
                    path,
                    kind,
                    ticket,
                    automation_command,
                    deadline,
                    save_destination,
                    save_completed,
                    worker,
                });
                self.data_status = format!("Opening {display_path}…");
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_session_load_tasks(&mut self) {
        if self.session_load_tasks.is_empty() {
            return;
        }
        let mut index = 0;
        while index < self.session_load_tasks.len() {
            if self.session_load_tasks[index]
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                let task = &mut self.session_load_tasks[index];
                task.deadline = None;
                self.document_state.cancel_load(task.ticket);
                if let Some(command) = task.automation_command.take() {
                    self.finish_automation_command(
                        command,
                        Err((
                            AutomationErrorCode::Timeout,
                            "project open timed out; late result will not be applied".into(),
                        )),
                    );
                }
            }
            if !self.session_load_tasks[index].worker.is_finished() {
                index += 1;
                continue;
            }
            self.observe_document_changes();
            let task = self.session_load_tasks.remove(index);
            let path = task.path;
            // A successful save-before-open remains a real save even if parsing
            // the replacement fails. It still only marks its captured revision.
            if task
                .save_completed
                .load(std::sync::atomic::Ordering::Acquire)
                && self.document_state.complete_save(task.ticket)
            {
                self.current_session_path.clone_from(&task.save_destination);
            }
            // Automation asked for an exact replacement and keeps the strict revision check. A
            // GUI open was already confirmed through the unsaved-changes gate, so revisions
            // bumped by unrelated background work while the file was read do not reject it.
            let can_commit = if task.automation_command.is_some() {
                self.document_state.load_can_commit(task.ticket)
            } else {
                self.document_state.gui_load_can_commit(task.ticket)
            };
            if !can_commit {
                let current = self.document_state.current_load(task.ticket);
                let _ = task.worker.join();
                if current {
                    self.data_status =
                        "Project changed while opening; retry to confirm replacement".into();
                }
                if let Some(command) = task.automation_command {
                    self.finish_automation_command(
                        command,
                        Err((
                            if current {
                                AutomationErrorCode::UnsavedChanges
                            } else {
                                AutomationErrorCode::InvalidState
                            },
                            "project open was superseded or the document changed while loading"
                                .into(),
                        )),
                    );
                }
                continue;
            }
            let mut applied = false;
            let mut failure_code = AutomationErrorCode::InvalidState;
            match task.worker.join() {
                Ok(Ok(loaded)) => {
                    let LoadedSession {
                        document,
                        igv_report,
                    } = loaded;
                    let unresolved = document.unresolved_local_sources();
                    match self.apply_session(&document) {
                        Ok(()) => {
                            applied = true;
                            self.bump_workspace_revision();
                            self.igv_import_report = igv_report;
                            match task.kind {
                                SessionLoadKind::Native => {
                                    self.document_state.mark_saved_baseline();
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
                            self.report_error(format!(
                                "Could not apply project {}: {error}",
                                path.display()
                            ));
                        }
                    }
                }
                Ok(Err(error)) => {
                    failure_code = AutomationErrorCode::Io;
                    self.report_error(format!("Could not open {}: {error}", path.display()));
                }
                Err(_) => {
                    failure_code = AutomationErrorCode::Internal;
                    self.report_error(format!(
                        "Project-load worker failed while reading {}",
                        path.display()
                    ));
                }
            }
            if let Some(command) = task.automation_command {
                let result = if applied {
                    Ok((
                        self.data_status.clone(),
                        serde_json::json!({
                            "application_instance_id": self.application_instance_id,
                            "workspace_revision": self.workspace_revision,
                            "settled": self.automation_is_settled(),
                            "panels": self.automation_workspace_snapshot().panels,
                        }),
                    ))
                } else {
                    Err((failure_code, self.data_status.clone()))
                };
                self.finish_automation_command(command, result);
            }
            self.mark_display_dirty("explicit display invalidation");
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
            if status.starts_with("Exported ") {
                self.data_status = status;
            } else {
                self.report_error(status);
            }
            if let (Some(command_index), Some(outcome)) = (automation_command, automation_outcome) {
                self.finish_automation_command(command_index, outcome);
            }
            self.mark_display_dirty("explicit display invalidation");
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
            self.mark_display_dirty("explicit display invalidation");
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
