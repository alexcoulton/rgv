use super::super::*;

impl Viewer {
    pub(crate) fn persist_settings(&mut self) {
        self.remember_current_locus();
        if let Err(error) = self.settings.write(&self.settings_path) {
            tracing::warn!(
                path = %self.settings_path.display(),
                %error,
                "could not persist desktop settings"
            );
        }
    }

    pub(crate) fn handle_dropped_file(&mut self, path: &Path) {
        let extension = path
            .extension()
            .map(|value| value.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "bam" | "cram" => self.open_alignment_source(path),
            "fa" | "fasta" | "fna" => self.open_reference_source(path),
            "rgvgenome" => {
                if let Err(error) = self.open_genome_configuration(path) {
                    self.data_status = error.to_string();
                }
            }
            "json" if GenomeConfiguration::read(path).is_ok() => {
                if let Err(error) = self.open_genome_configuration(path) {
                    self.data_status = error.to_string();
                }
            }
            "json" => self.start_session_load(path.to_path_buf(), SessionLoadKind::Native),
            "xml" => self.start_session_load(path.to_path_buf(), SessionLoadKind::IgvImport),
            _ => {
                if let Err(error) = self.add_track_source(path) {
                    self.data_status = error.to_string();
                }
            }
        }
    }

    pub(crate) fn add_track_source(&mut self, path: &Path) -> Result<()> {
        let (kind, track_kind) = inferred_desktop_track_kind(path)?;
        let source = track_source(path);
        let name = source_display_name(Some(&source), "Genomic track");
        let format = match kind {
            TrackSpecKind::BigGenePred => Some(TrackFormat::BigGenePred),
            TrackSpecKind::BigNarrowPeak => Some(TrackFormat::BigNarrowPeak),
            TrackSpecKind::TdfChannel { index } => Some(TrackFormat::TdfChannel { index }),
            _ => None,
        };
        self.add_configured_track_source(path, kind, track_kind, name, format)
    }

    pub(crate) fn add_configured_track_source(
        &mut self,
        path: &Path,
        kind: TrackSpecKind,
        track_kind: TrackKind,
        name: String,
        format: Option<TrackFormat>,
    ) -> Result<()> {
        let id = TrackId::new(
            self.tracks
                .tracks()
                .iter()
                .map(|track| track.id.get())
                .chain(
                    self.alignment_tracks
                        .iter()
                        .map(|track| track.config.id.get()),
                )
                .max()
                .unwrap_or_default()
                .saturating_add(1),
        );
        let specification = TrackSpec {
            id,
            name: name.clone(),
            path: path.to_path_buf(),
            kind,
        };
        if let Some(engine) = &self.track_engine {
            engine.try_add(specification)?;
        } else {
            self.track_engine = Some(TrackEngine::spawn(vec![specification], 8));
        }
        let mut config = TrackConfig::new(id, name, track_kind, track_source(path));
        config.format = format;
        let source_label = config.source.redacted_label();
        self.tracks.push(config);
        self.synthetic = false;
        self.data_status = format!("Opening track {source_label}…");
        Ok(())
    }

    pub(crate) fn expand_tdf_channels(
        &mut self,
        parent_id: TrackId,
        track_names: &[String],
        selected_index: usize,
    ) -> Result<()> {
        if track_names.is_empty() || selected_index >= track_names.len() {
            anyhow::bail!("TDF source returned an invalid embedded-channel catalogue");
        }
        let Some(parent_position) = self
            .tracks
            .tracks()
            .iter()
            .position(|track| track.id == parent_id)
        else {
            return Ok(());
        };
        let parent = self.tracks.tracks()[parent_position].clone();
        if matches!(parent.format, Some(TrackFormat::TdfChannel { .. })) {
            return Ok(());
        }
        let path = source_path(&parent.source);
        let source_label = parent.source.redacted_label();
        let next_id = self
            .tracks
            .tracks()
            .iter()
            .map(|track| track.id.get())
            .chain(
                self.alignment_tracks
                    .iter()
                    .map(|track| track.config.id.get()),
            )
            .max()
            .unwrap_or_default()
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("TDF channel track ID space is exhausted"))?;
        let expansion =
            build_tdf_channel_expansion(&parent, &path, track_names, selected_index, next_id)?;
        if !expansion.specifications.is_empty() {
            let engine = self
                .track_engine
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("TDF track engine is unavailable"))?;
            engine.try_add_many(expansion.specifications)?;
        }
        let parent = &mut self.tracks.tracks_mut()[parent_position];
        parent.name = expansion.selected_name;
        parent.format = Some(TrackFormat::TdfChannel {
            index: selected_index,
        });
        self.tracks.insert_after(parent_id, expansion.children);
        if selected_index > 0 {
            self.tracks
                .move_track(parent_id, parent_position.saturating_add(selected_index));
        }
        self.data_status = format!(
            "Opened {} embedded TDF channels from {}",
            track_names.len(),
            source_label
        );
        Ok(())
    }

    pub(crate) fn apply_primary_alignment_config(
        &mut self,
        alignment: Option<&SessionAlignmentTrack>,
        workspace_limit: usize,
    ) -> Result<()> {
        let filters = alignment.map_or_else(SessionFilters::default, |track| track.filters.clone());
        let (filter_editor, active_filters) = filter_state_from_session(&filters)?;
        self.style_tag_editor = persisted_style_tag(
            filters.alignment_color_mode,
            filters.alignment_group_mode,
            filters.alignment_sort_mode,
        );
        self.workspace_alignment_limit = workspace_limit.max(1);
        let maximum_alignments = alignment
            .map_or(self.workspace_alignment_limit, |track| {
                track.maximum_alignments
            })
            .min(self.workspace_alignment_limit)
            .max(1);
        if let Some(track) = self.alignment_tracks.first_mut()
            && alignment.is_some_and(|alignment| alignment.id == track.config.id)
        {
            track.config.filters = filters;
            track.config.maximum_alignments = maximum_alignments;
            track.filter_editor = filter_editor;
            track.active_filters = active_filters;
        } else {
            self.empty_alignment.filters = filters;
            self.empty_alignment.filter_editor = filter_editor;
            self.empty_alignment.active_filters = active_filters;
            self.empty_alignment.name =
                alignment.map_or_else(|| "Alignments".to_owned(), |track| track.name.clone());
            self.empty_alignment.visible = alignment.is_none_or(|track| track.visible);
            self.empty_alignment.collapsed = alignment.is_some_and(|track| track.collapsed);
            self.empty_alignment.maximum_alignments = maximum_alignments;
        }
        Ok(())
    }

    pub(crate) fn apply_session(&mut self, session: &SessionDocument) -> Result<()> {
        self.reference_open_error = None;
        self.automation_auxiliary_open_state.clear();
        self.unresolved_session_sources = session.unresolved_local_sources();
        self.promoted_read = None;
        self.read_name_editor.clear();
        self.variant_sample_search.clear();
        self.variant_settings_sample_search.clear();
        let alignment = primary_alignment_track(session).cloned();
        self.apply_primary_alignment_config(alignment.as_ref(), session.maximum_alignments)?;
        self.reference_source.clone_from(&session.reference);
        self.reference_annotation_height = None;
        self.remote_reference_index_url
            .clone_from(&session.remote_reference_index_url);
        self.reference_reverse_complement = session.reference_reverse_complement;
        self.reference_translation_frames
            .clone_from(&session.reference_translation_frames);
        self.reference_genetic_code = session.reference_genetic_code;
        self.regions_of_interest
            .clone_from(&session.regions_of_interest);
        self.review_items.clone_from(&session.review_items);
        self.sample_metadata.clone_from(&session.sample_metadata);
        self.metadata_filter_column = 0;
        self.metadata_filter_value = None;
        self.metadata_selected_tracks.clear();
        self.metadata_remove_armed = false;
        if let Some(name) = &session.genome_name {
            self.assembly_name.clone_from(name);
        }
        self.assembly_identity
            .clone_from(&session.assembly_identity);
        self.assembly
            .identity
            .clone_from(&session.assembly_identity);
        self.cytoband_path = match &session.cytobands {
            Some(TrackSource::Local { path }) => Some(PathBuf::from(path)),
            Some(TrackSource::Remote { .. }) => {
                anyhow::bail!("remote session cytoband files are not yet supported")
            }
            None => None,
        };
        self.contig_aliases.clone_from(&session.contig_aliases);
        let (specifications, tracks) = session_track_specs(session);
        self.tracks = tracks;
        self.automation_auxiliary_open_state.extend(
            self.tracks
                .tracks()
                .iter()
                .map(|track| (track.id, AutomationAuxiliaryOpenState::Pending)),
        );
        self.track_engine =
            (!specifications.is_empty()).then(|| TrackEngine::spawn(specifications, 8));
        self.panels = PanelWorkspace::from_session(
            session,
            self.canvas_geometry.genomic.width(),
            self.canvas_geometry.viewport.top(),
        )?;
        self.reference_dictionary = None;
        self.reference_engine = if let Some(source) = &self.reference_source {
            let engine = ReferenceEngine::spawn(8);
            engine.try_open_with_options(
                source_path(source),
                reference_open_options(self.remote_reference_index_url.clone()),
            )?;
            Some(engine)
        } else {
            None
        };
        self.refresh_active_panel_context();
        self.layout_cache.clear();
        self.replace_alignment_track_controllers(session.alignment_tracks.clone())?;
        if alignment.is_some() {
            "Opening alignment tracks…".clone_into(&mut self.data_status);
        } else {
            self.panels.active_mut().pending_queries.alignment = false;
            self.synthetic = false;
            self.panels.active_mut().display_list = DisplayList::default();
            self.rebuild_all_panel_displays();
            self.request_all_panel_queries();
            self.display_dirty = true;
        }
        Ok(())
    }

    pub(crate) fn live_session_filters(&self) -> SessionFilters {
        let mut filters = self.primary_alignment_filters().clone();
        filters.minimum_mapping_quality = self
            .primary_active_filters()
            .alignment
            .minimum_mapping_quality;
        filters.include_secondary = self.primary_active_filters().include_secondary;
        filters.include_supplementary = self.primary_active_filters().include_supplementary;
        filters.auxiliary_predicates = self
            .primary_filter_editor()
            .expression
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        filters
    }

    pub(crate) fn session_document(&self) -> SessionDocument {
        let interval = self.panels.active().viewport.interval();
        let locus = SessionLocus {
            contig: self.panels.active().contig_name.clone(),
            start: interval.start(),
            end: interval.end(),
        };
        let panels = self
            .panels
            .iter()
            .map(|panel| {
                let interval = panel.viewport.interval();
                let mut saved = SessionViewPanel::new(
                    panel.id,
                    SessionLocus {
                        contig: panel.contig_name.clone(),
                        start: interval.start(),
                        end: interval.end(),
                    },
                );
                saved.vertical_scroll_points = panel.vertical_scroll_points;
                saved.annotation_scroll_points = panel.annotation_scroll_points;
                saved.vertical_content_origin_points = panel.vertical_content_origin_points;
                saved
            })
            .collect();
        let mut alignment_tracks = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.clone())
            .collect::<Vec<_>>();
        if let Some(primary) = alignment_tracks.first_mut() {
            primary.filters = self.live_session_filters();
        }
        SessionDocument {
            schema_version: rgv_core::SESSION_SCHEMA_VERSION,
            locus,
            panels,
            active_panel: self.panels.active().id,
            alignment_tracks,
            reference: self.reference_source.clone(),
            remote_reference_index_url: self.remote_reference_index_url.clone(),
            genome_name: Some(self.assembly_name.clone()),
            assembly_identity: self.assembly_identity.clone(),
            cytobands: self.cytoband_path.as_deref().map(track_source),
            reference_reverse_complement: self.reference_reverse_complement,
            reference_translation_frames: self.reference_translation_frames.clone(),
            reference_genetic_code: self.reference_genetic_code,
            regions_of_interest: self.regions_of_interest.clone(),
            review_items: self.review_items.clone(),
            sample_metadata: self.sample_metadata.clone(),
            contig_aliases: self.contig_aliases.clone(),
            tracks: self.tracks.tracks().to_vec(),
            maximum_alignments: self.workspace_alignment_limit,
        }
    }
}
