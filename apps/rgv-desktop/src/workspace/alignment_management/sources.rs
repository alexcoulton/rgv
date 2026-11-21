use crate::sources::{
    reference_open_options, source_display_name, source_path, track_source,
    upsert_primary_alignment_config,
};
use crate::ui::TrackFrameKey;
use crate::workspace::alignments::{
    AlignmentTrackController, filter_state_from_session, spawn_alignment_track_controller,
};
use crate::{
    RemoteSourceKind, RemoteSourceRequest, SYNTHETIC_ALIGNMENT_TRACK_ID, UiPanel, Viewer,
    clear_removed_alignment_panel_state,
};
use anyhow::Result;
use rgv_alignment_scene::AlignmentPanelSceneStore;
use rgv_core::{
    GenomeConfiguration, MAXIMUM_ALIGNMENT_TRACKS, PanelId, SessionAlignmentTrack, TrackId,
    TrackSource,
};
use rgv_engine::{AlignmentSceneEngine, ReferenceEngine};
use rgv_layout::DisplayList;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

impl Viewer {
    pub(crate) fn alignment_track_limit_reached(&mut self) -> bool {
        if self.alignment_tracks.len() >= MAXIMUM_ALIGNMENT_TRACKS {
            self.data_status =
                format!("A workspace supports at most {MAXIMUM_ALIGNMENT_TRACKS} alignment tracks");
            return true;
        }
        false
    }

    pub(crate) fn add_alignment_track(
        &mut self,
        source: TrackSource,
        opened_name: &str,
        remote_index_url: Option<String>,
    ) {
        let mut configs = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.clone())
            .collect::<Vec<_>>();
        let next_id = self.allocate_track_id();
        let mut config = SessionAlignmentTrack::new(next_id, opened_name.to_owned(), source);
        config.remote_index_url = remote_index_url;
        config.maximum_alignments = config
            .maximum_alignments
            .min(self.workspace_alignment_limit)
            .max(1);
        configs.push(config);
        match self.replace_alignment_track_controllers(configs) {
            Ok(()) => {
                self.invalidate_navigation_contract("alignment source set changed");
                self.begin_all_panel_refresh_transactions();
                self.panels.active_mut().pending_queries.alignment = false;
                self.synthetic = false;
                self.data_status =
                    format!("Opening {opened_name} and rebalancing alignment-track caches…");
                self.rebuild_genomic_display(self.panels.active().id);
                self.mark_display_dirty("explicit display invalidation");
            }
            Err(error) => self.report_error(error.to_string()),
        }
    }

    pub(crate) fn open_alignment_source(&mut self, path: &Path) {
        self.open_alignment_source_with_remote_index(path, None);
    }

    pub(crate) fn open_remote_source(&mut self, request: RemoteSourceRequest) -> Result<()> {
        match request.kind {
            RemoteSourceKind::Alignment => {
                if let Some(reference_url) = request.reference_url {
                    self.open_reference_source_with_remote_index(
                        Path::new(&reference_url),
                        request.reference_index_url,
                    );
                }
                self.open_alignment_source_with_remote_index(
                    Path::new(&request.source_url),
                    request.index_url,
                );
            }
            RemoteSourceKind::Reference => {
                self.open_reference_source_with_remote_index(
                    Path::new(&request.source_url),
                    request.index_url,
                );
            }
            RemoteSourceKind::IndexedTrack => {
                self.add_track_source(Path::new(&request.source_url))?;
            }
        }
        Ok(())
    }

    pub(crate) fn open_alignment_source_with_remote_index(
        &mut self,
        path: &Path,
        remote_index_url: Option<String>,
    ) {
        if self.alignment_track_limit_reached() {
            return;
        }
        let source = track_source(path);
        let opened_name = source_display_name(Some(&source), "Alignments");
        if !self.alignment_tracks.is_empty() {
            self.add_alignment_track(source, &opened_name, remote_index_url);
            return;
        }
        let source_changed = self.primary_alignment_source() != Some(&source);
        let name = if source_changed {
            opened_name
        } else {
            self.primary_alignment_name().to_owned()
        };
        let mut configs = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.clone())
            .collect::<Vec<_>>();
        let config = upsert_primary_alignment_config(
            &mut configs,
            self.tracks.tracks(),
            source.clone(),
            &name,
        );
        config.remote_index_url = remote_index_url;
        config.filters = self.primary_alignment_filters().clone();
        config.maximum_alignments = self
            .primary_alignment_limit()
            .min(self.workspace_alignment_limit)
            .max(1);
        let cache_bytes = NonZeroUsize::new(self.alignment_scene_budget.budget_bytes.max(1))
            .expect("scene budget is non-zero");
        let controller = spawn_alignment_track_controller(
            configs.remove(0),
            cache_bytes,
            self.alignment_scene_ledger.clone(),
            self.reference_source.as_ref().map(source_path),
            self.remote_reference_index_url.clone(),
            &self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>(),
        );
        match controller {
            Ok(controller) => {
                let interval = self.panels.active().viewport.interval();
                self.panels.active_mut().requested_locus = Some(format!(
                    "{}:{}-{}",
                    self.panels.active().contig_name,
                    interval.start() + 1,
                    interval.end()
                ));
                self.panels.active().generation_clock.advance();
                crate::workspace::alignments::connect_reference_slice_sinks(
                    self.reference_engine.as_ref(),
                    [&controller],
                );
                if self.alignment_tracks.is_empty() {
                    self.alignment_tracks.push(controller);
                } else {
                    self.alignment_tracks[0] = controller;
                }
                self.invalidate_navigation_contract("alignment source changed");
                self.begin_all_panel_refresh_transactions();
                self.reconcile_track_presentation();
                self.panels.active_mut().pending_queries.alignment = false;
                self.panels.active_mut().selected_chain = None;
                self.panels.active_mut().display_list = DisplayList::default();
                self.synthetic = false;
                self.data_status = format!("Opening {}…", source.redacted_label());
                self.mark_display_dirty("explicit display invalidation");
            }
            Err(error) => self.report_error(error.to_string()),
        }
    }

    pub(crate) fn open_reference_source(&mut self, path: &Path) {
        self.open_reference_source_with_remote_index(path, None);
    }

    pub(crate) fn open_reference_source_with_remote_index(
        &mut self,
        path: &Path,
        remote_index_url: Option<String>,
    ) {
        if let Err(error) = self.ensure_engine_retirement_capacity(2) {
            self.report_error(error.to_string());
            return;
        }
        self.reference_open_error = None;
        self.remote_reference_index_url = remote_index_url;
        let source = track_source(path);
        let source_label = source.redacted_label();
        let engine = ReferenceEngine::spawn(8);
        if let Err(error) = engine.try_open_with_options(
            path.to_path_buf(),
            reference_open_options(self.remote_reference_index_url.clone()),
        ) {
            self.report_error(error.to_string());
            if let Some(worker) = engine.retire() {
                self.engine_retirement_tasks.push(worker);
            }
            return;
        }
        self.reference_source = Some(source);
        self.reference_dictionary = None;
        self.reference_revision = None;
        self.reference_remote_io_stats = None;
        self.retire_reference_engine();
        // The retired engine took its slice sinks with it: reconnect every scene engine.
        crate::workspace::alignments::connect_reference_slice_sinks(
            Some(&engine),
            &self.alignment_tracks,
        );
        self.reference_engine = Some(engine);
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            if let Some(panel) = self.panels.get_mut(id) {
                let interval = panel.viewport.interval();
                panel.requested_locus = Some(format!(
                    "{}:{}-{}",
                    panel.contig_name,
                    interval.start().saturating_add(1),
                    interval.end()
                ));
                panel.generation_clock.advance();
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
        }
        if !self.alignment_tracks.is_empty() {
            let configs = self
                .alignment_tracks
                .iter()
                .map(|track| track.config.clone())
                .collect::<Vec<_>>();
            if let Err(error) = self.replace_alignment_track_controllers(configs) {
                self.report_error(error.to_string());
                return;
            }
        }
        self.synthetic = false;
        self.primary_alignment_filters_mut().show_reference = true;
        self.data_status = format!("Opening reference genome {source_label}…");
        self.invalidate_navigation_contract("reference source changed");
        self.begin_all_panel_refresh_transactions();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
    }

    /// Opens an RGV genome configuration, or an IGV genome description (loaded in the
    /// background because its resources may need downloading).
    pub(crate) fn open_genome_configuration(&mut self, path: &Path) -> Result<()> {
        let configuration = match GenomeConfiguration::read(path) {
            Ok(configuration) => configuration,
            Err(error) => {
                if crate::workspace::genomes::is_igv_genome_description(path) {
                    self.start_genome_load(path.to_string_lossy().into_owned());
                    return Ok(());
                }
                return Err(error.into());
            }
        };
        self.apply_genome_configuration(&configuration)?;
        self.data_status = format!("Opening genome configuration {}…", path.display());
        Ok(())
    }

    pub(crate) fn prepare_alignment_track_controllers(
        &mut self,
        configs: Vec<SessionAlignmentTrack>,
        reference_path: Option<PathBuf>,
        remote_reference_index_url: Option<String>,
        panel_ids: &[PanelId],
    ) -> Result<Vec<AlignmentTrackController>> {
        for config in &configs {
            filter_state_from_session(&config.filters)?;
        }
        let cache_bytes = NonZeroUsize::new(self.alignment_scene_budget.budget_bytes.max(1))
            .expect("scene budget is non-zero");
        let mut controllers = Vec::with_capacity(configs.len());
        for config in configs {
            match spawn_alignment_track_controller(
                config,
                cache_bytes,
                self.alignment_scene_ledger.clone(),
                reference_path.clone(),
                remote_reference_index_url.clone(),
                panel_ids,
            ) {
                Ok(controller) => controllers.push(controller),
                Err(error) => {
                    for controller in controllers {
                        self.retire_alignment_track_controller(controller);
                    }
                    return Err(error);
                }
            }
        }
        Ok(controllers)
    }

    pub(crate) fn install_alignment_track_controllers(
        &mut self,
        controllers: Vec<AlignmentTrackController>,
    ) {
        // Covers a session load too: its new reference engine is installed before this call.
        crate::workspace::alignments::connect_reference_slice_sinks(
            self.reference_engine.as_ref(),
            &controllers,
        );
        let replaced = std::mem::replace(&mut self.alignment_tracks, controllers);
        for controller in replaced {
            self.retire_alignment_track_controller(controller);
        }
        self.reset_alignment_scene_panel_authority();
        self.reconcile_track_presentation();
    }

    pub(crate) fn replace_alignment_track_controllers(
        &mut self,
        configs: Vec<SessionAlignmentTrack>,
    ) -> Result<()> {
        self.poll_alignment_retirement_tasks();
        anyhow::ensure!(
            self.alignment_retirement_tasks.len() <= rgv_core::MAXIMUM_ALIGNMENT_TRACKS,
            "Previous alignment sources are still closing; retry shortly"
        );
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let controllers = self.prepare_alignment_track_controllers(
            configs,
            self.reference_source.as_ref().map(source_path),
            self.remote_reference_index_url.clone(),
            &panel_ids,
        )?;
        self.install_alignment_track_controllers(controllers);
        Ok(())
    }

    pub(super) fn reset_alignment_scene_panel_authority(&mut self) {
        let track_ids = self
            .alignment_tracks
            .iter()
            .filter(|track| track.scene_engine.is_some())
            .map(|track| track.config.id)
            .collect::<Vec<_>>();
        self.alignment_panel_scene_stores = self
            .panels
            .iter()
            .map(|panel| {
                (
                    panel.id,
                    AlignmentPanelSceneStore::new(track_ids.iter().copied()),
                )
            })
            .collect();
        self.alignment_presented_epoch = None;
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_publication_retry = None;
        self.alignment_capacity_reclaim = None;
        self.alignment_gpu_speculation_gate.reset();
        self.alignment_scene_publication_authority.reset();
    }

    pub(crate) fn retire_alignment_track_controller(
        &mut self,
        controller: AlignmentTrackController,
    ) {
        for projection in controller.projections.values() {
            projection.advance_scene_generation();
        }
        let AlignmentTrackController { scene_engine, .. } = controller;
        if let Some(task) = scene_engine.and_then(AlignmentSceneEngine::retire) {
            self.alignment_retirement_tasks.push(task);
        }
    }

    pub(crate) fn activate_first_alignment_track(&mut self) -> Result<()> {
        if let Some((config, filter_editor, active_filters)) =
            self.alignment_tracks.first().map(|track| {
                (
                    track.config.clone(),
                    track.filter_editor.clone(),
                    track.active_filters.clone(),
                )
            })
        {
            self.apply_primary_alignment_config(Some(&config), self.workspace_alignment_limit)?;
            let primary = self
                .alignment_tracks
                .first_mut()
                .expect("primary alignment still exists");
            primary.filter_editor = filter_editor;
            primary.active_filters = active_filters;
        } else {
            self.apply_primary_alignment_config(None, self.workspace_alignment_limit)?;
        }
        Ok(())
    }

    pub(crate) fn remove_alignment_track(&mut self, id: TrackId) -> Result<bool> {
        if id == SYNTHETIC_ALIGNMENT_TRACK_ID {
            return Ok(false);
        }
        let Some(index) = self
            .alignment_tracks
            .iter()
            .position(|track| track.config.id == id)
        else {
            return Ok(false);
        };
        let removed_was_primary = index == 0;
        let removed_track = self.alignment_tracks.remove(index);
        self.retire_alignment_track_controller(removed_track);
        self.reset_alignment_scene_panel_authority();
        self.track_presentation.remove_source(id);
        self.normalize_alignment_track_groups();
        if self
            .promoted_read
            .is_some_and(|(track_id, _)| track_id == id)
        {
            self.promoted_read = None;
        }
        self.metadata_selected_tracks.remove(&id);
        if matches!(
            self.open_panel,
            Some(UiPanel::TrackSettings(TrackFrameKey::Alignment(track_id))) if track_id == id
        ) {
            self.open_panel = None;
        }
        if self.resizing_annotation_track.is_some_and(
            |(key, _)| matches!(key, TrackFrameKey::Alignment(track_id) if track_id == id),
        ) {
            self.resizing_annotation_track = None;
        }
        if self
            .resizing_read_track
            .is_some_and(|(track_id, _)| track_id == id)
        {
            self.resizing_read_track = None;
        }
        if self
            .vertical_overview_drag
            .is_some_and(|drag| drag.track_id == id)
        {
            self.vertical_overview_drag = None;
        }
        if removed_was_primary {
            self.activate_first_alignment_track()?;
        }
        for panel in self.panels.iter_mut() {
            clear_removed_alignment_panel_state(panel, id, removed_was_primary);
        }
        for track in &self.alignment_tracks {
            for projection in track.projections.values() {
                projection.advance_scene_generation();
            }
        }
        self.invalidate_navigation_contract("alignment source removed");
        self.begin_all_panel_refresh_transactions();
        self.request_all_alignment_queries();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
        Ok(true)
    }
}
