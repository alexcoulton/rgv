use super::super::*;

impl Viewer {
    pub(crate) fn request_alignment_query(&mut self) {
        let panel_id = self.panels.active().id;
        for track in &mut self.alignment_tracks {
            track.projection_mut(panel_id).pending_query =
                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_));
        }
        self.panels.active_mut().pending_queries.alignment =
            self.alignment_tracks.iter().any(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.pending_query)
            });
        self.dispatch_alignment_query();
    }

    pub(crate) fn dispatch_alignment_query(&mut self) {
        if !self.panels.active().pending_queries.alignment {
            return;
        }
        if self.alignment_tracks.is_empty() {
            self.panels.active_mut().pending_queries.alignment = false;
            return;
        }
        let viewport = self.panels.active().viewport;
        let resolution = AlignmentQueryResolution::for_viewport(viewport);
        let panel_id = self.panels.active().id;
        if !resolution.requires_alignment_records() {
            let mut settled = 0_usize;
            for track in &mut self.alignment_tracks {
                match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => {}
                    AlignmentTrackLifecycle::Ready => {
                        track
                            .projection_mut(panel_id)
                            .settle_index_only_overview(resolution);
                        settled = settled.saturating_add(1);
                    }
                    AlignmentTrackLifecycle::Failed(_) => {
                        let projection = track.projection_mut(panel_id);
                        projection.pending_query = false;
                        projection.index_only_overview = false;
                    }
                }
            }
            self.panels.active_mut().pending_queries.alignment =
                self.alignment_tracks.iter().any(|track| {
                    track
                        .projection(panel_id)
                        .is_some_and(|projection| projection.pending_query)
                });
            if settled > 0 {
                self.data_status = format!(
                    "Index-only overview for {settled} alignment track(s); no records requested"
                );
            }
            return;
        }
        let record_requests = self
            .alignment_tracks
            .iter()
            .map(|track| {
                if !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_)) {
                    track.config.maximum_alignments
                } else {
                    0
                }
            })
            .collect::<Vec<_>>();
        let panel_record_budget =
            (self.workspace_alignment_limit / self.panels.len().max(1)).max(1);
        let record_allocations = allocate_workspace_records(panel_record_budget, &record_requests);
        let viewport_interval = self.panels.active().viewport.interval();
        let contig_name = self.panels.active().contig_name.clone();
        let mut sent = 0_usize;
        for (track, record_allocation) in self.alignment_tracks.iter_mut().zip(record_allocations) {
            let Some(interval) = track.prepare_query_interval(
                panel_id,
                record_allocation,
                &contig_name,
                viewport_interval,
            ) else {
                continue;
            };
            let options = alignment_query_options(
                track,
                resolution.detail,
                viewport.bases_per_pixel(),
                record_allocation,
                resolution.coverage_bins,
                resolution.collect_alleles,
            );
            let (request, clock) = {
                let projection = track.projection_mut(panel_id);
                (
                    PanelGeneration::new(panel_id, projection.clock.current()),
                    projection.clock.clone(),
                )
            };
            match track.engine.try_query(request, clock, interval, options) {
                Ok(()) => {
                    let projection = track.projection_mut(panel_id);
                    projection.pending_query = false;
                    projection.begin_checking(Instant::now());
                    projection.last_query_resolution = Some(resolution);
                    sent = sent.saturating_add(1);
                }
                Err(EngineQueueError::Full) => {}
                Err(EngineQueueError::Disconnected) => {
                    track.projection_mut(panel_id).pending_query = false;
                    track.lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
            }
        }
        self.panels.active_mut().pending_queries.alignment =
            self.alignment_tracks.iter().any(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.pending_query)
            });
        if sent > 0 {
            self.data_status = format!("Loading current locus for {sent} alignment track(s)…");
        } else if self.panels.active().pending_queries.alignment {
            "Alignment queues busy; retrying the latest locus…".clone_into(&mut self.data_status);
        }
    }

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
        let next_id = configs
            .iter()
            .map(|track| track.id.get())
            .chain(self.tracks.tracks().iter().map(|track| track.id.get()))
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        let mut config =
            SessionAlignmentTrack::new(TrackId::new(next_id), opened_name.to_owned(), source);
        config.remote_index_url = remote_index_url;
        config.maximum_alignments = config
            .maximum_alignments
            .min(self.workspace_alignment_limit)
            .max(1);
        configs.push(config);
        match self.replace_alignment_track_controllers(configs) {
            Ok(()) => {
                self.invalidate_navigation_preview_contract("alignment source set changed");
                self.begin_all_panel_refresh_transactions();
                self.panels.active_mut().pending_queries.alignment = false;
                self.synthetic = false;
                self.data_status =
                    format!("Opening {opened_name} and rebalancing alignment-track caches…");
                self.rebuild_genomic_display();
                self.display_dirty = true;
            }
            Err(error) => self.data_status = error.to_string(),
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
        let cache_bytes = alignment_cache_share(
            self.cache_budgets.decoded_alignments,
            self.alignment_tracks.len().max(1),
        );
        let controller = spawn_alignment_track_controller(
            configs.remove(0),
            cache_bytes,
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
                if self.alignment_tracks.is_empty() {
                    self.alignment_tracks.push(controller);
                } else {
                    self.alignment_tracks[0] = controller;
                }
                self.invalidate_navigation_preview_contract("alignment source changed");
                self.begin_all_panel_refresh_transactions();
                self.reconcile_track_presentation();
                self.panels.active_mut().pending_queries.alignment = false;
                self.panels.active_mut().selected_chain = None;
                self.panels.active_mut().display_list = DisplayList::default();
                self.layout_cache.clear();
                self.synthetic = false;
                self.data_status = format!("Opening {}…", source.redacted_label());
                self.display_dirty = true;
            }
            Err(error) => self.data_status = error.to_string(),
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
        self.reference_open_error = None;
        self.remote_reference_index_url = remote_index_url;
        let source = track_source(path);
        let source_label = source.redacted_label();
        let engine = ReferenceEngine::spawn(8);
        if let Err(error) = engine.try_open_with_options(
            path.to_path_buf(),
            reference_open_options(self.remote_reference_index_url.clone()),
        ) {
            self.data_status = error.to_string();
            return;
        }
        self.reference_source = Some(source);
        self.reference_dictionary = None;
        self.reference_remote_io_stats = None;
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
                self.data_status = error.to_string();
                return;
            }
        }
        self.synthetic = false;
        self.primary_alignment_filters_mut().show_reference = true;
        self.data_status = format!("Opening reference genome {source_label}…");
        self.invalidate_navigation_preview_contract("reference source changed");
        self.begin_all_panel_refresh_transactions();
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
    }

    pub(crate) fn open_genome_configuration(&mut self, path: &Path) -> Result<()> {
        let configuration = GenomeConfiguration::read(path)?;
        let cytoband_path = match &configuration.cytobands {
            Some(TrackSource::Local { path }) => Some(PathBuf::from(path)),
            Some(TrackSource::Remote { .. }) => {
                anyhow::bail!("remote cytoband files are not yet supported")
            }
            None => None,
        };
        let annotations = configuration
            .default_annotations
            .iter()
            .map(|annotation| {
                track_spec_kind(annotation.kind, &annotation.source, annotation.format)
                    .map(|kind| {
                        (
                            source_path(&annotation.source),
                            kind,
                            annotation.kind,
                            annotation.name.clone(),
                            annotation.format,
                        )
                    })
                    .with_context(|| format!("unsupported default annotation {}", annotation.name))
            })
            .collect::<Result<Vec<_>>>()?;

        self.assembly_name.clone_from(&configuration.name);
        self.assembly_identity
            .clone_from(&configuration.assembly_identity);
        self.assembly
            .identity
            .clone_from(&configuration.assembly_identity);
        self.assembly.default_annotation_count = configuration.default_annotations.len();
        self.contig_aliases
            .clone_from(&configuration.contig_aliases);
        self.cytoband_path = cytoband_path;
        let reference = source_path(&configuration.reference);
        self.open_reference_source(&reference);
        for (annotation_path, kind, track_kind, name, format) in annotations {
            self.add_configured_track_source(&annotation_path, kind, track_kind, name, format)?;
        }
        self.data_status = format!("Opening genome configuration {}…", path.display());
        Ok(())
    }

    pub(crate) fn replace_alignment_track_controllers(
        &mut self,
        configs: Vec<SessionAlignmentTrack>,
    ) -> Result<()> {
        let cache_bytes =
            alignment_cache_share(self.cache_budgets.decoded_alignments, configs.len());
        let reference_path = self.reference_source.as_ref().map(source_path);
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let mut controllers = Vec::with_capacity(configs.len());
        for config in configs {
            controllers.push(spawn_alignment_track_controller(
                config,
                cache_bytes,
                reference_path.clone(),
                self.remote_reference_index_url.clone(),
                &panel_ids,
            )?);
        }
        let replaced = std::mem::replace(&mut self.alignment_tracks, controllers);
        for controller in replaced {
            self.retire_alignment_track_controller(controller);
        }
        self.reconcile_track_presentation();
        Ok(())
    }

    pub(crate) fn retire_alignment_track_controller(
        &mut self,
        controller: AlignmentTrackController,
    ) {
        for projection in controller.projections.values() {
            projection.clock.advance();
        }
        let AlignmentTrackController { engine, .. } = controller;
        if let Some(task) = engine.retire() {
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
        self.layout_cache.clear();
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
        for track in &mut self.alignment_tracks {
            for projection in track.projections.values_mut() {
                projection.clock.advance();
                projection.pending_query =
                    !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_));
            }
        }
        self.invalidate_navigation_preview_contract("alignment source removed");
        self.begin_all_panel_refresh_transactions();
        self.request_all_alignment_queries();
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
        Ok(true)
    }
}
