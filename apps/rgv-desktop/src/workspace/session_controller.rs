use crate::Viewer;
use crate::app::tasks::SessionLoadKind;
use crate::automation::AutomationAuxiliaryOpenState;
use crate::display::alignment::persisted_style_tag;
use crate::launch::LaunchOptions;
use crate::sources::{
    DroppedFileKind, DroppedJsonKind, build_tdf_channel_expansion, classify_dropped_file,
    classify_dropped_json, inferred_desktop_track_kind, primary_alignment_track,
    reference_open_options, session_track_specs, source_display_name, source_path,
    spawn_track_engine, spawn_track_engine_without_aliases, track_source,
};
use crate::workspace::alignments::filter_state_from_session;
use crate::workspace::navigation::{ImportedLocusRequest, RequestedLocus, resolve_requested_locus};
use crate::workspace::panels::{PanelWorkspace, ViewPanelController};
use anyhow::Result;
use rgv_core::{
    PanelId, ReferenceDictionary, SessionAlignmentTrack, SessionDocument, SessionFilters,
    SessionLocus, SessionReadPanelState, SessionViewPanel, TrackConfig, TrackFormat, TrackId,
    TrackKind, TrackSource, WorkspacePresentation,
};
use rgv_engine::{ReferenceEngine, TrackSpec, TrackSpecKind};
use rgv_layout::DisplayList;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Pure desktop preparation: no live state or worker ownership changes until
/// every capability, filter and panel construction check has succeeded.
fn prepare_session_geometry(
    session: &SessionDocument,
    width: f32,
    top: f32,
) -> Result<(PanelWorkspace, Option<PathBuf>)> {
    session.to_json()?; // Includes core presentation, source and filter validation.
    let cytobands = match &session.cytobands {
        Some(TrackSource::Local { path }) => Some(PathBuf::from(path)),
        Some(TrackSource::Remote { .. }) => {
            anyhow::bail!("remote session cytoband files are not yet supported")
        }
        None => None,
    };
    for track in &session.alignment_tracks {
        filter_state_from_session(&track.filters)?;
    }
    Ok((
        PanelWorkspace::from_session(session, width, top)?,
        cytobands,
    ))
}

/// The panel part of a session document: the focused panel's locus, every panel in order, and
/// which one has focus. This is the persisted projection of the panel workspace, so the action
/// tests compare it to decide whether two front ends left the same document.
pub(crate) fn session_panel_documents(
    panels: &PanelWorkspace,
) -> (SessionLocus, Vec<SessionViewPanel>, PanelId) {
    let locus_of = |panel: &ViewPanelController| {
        let interval = panel.viewport.interval();
        SessionLocus {
            contig: panel.contig_name.clone(),
            start: interval.start(),
            end: interval.end(),
        }
    };
    let saved = panels
        .iter()
        .map(|panel| {
            let mut saved = SessionViewPanel::new(panel.id, locus_of(panel));
            saved.vertical_scroll_points = panel.vertical_scroll_points;
            // Keep the existing per-panel session field as a compatibility
            // projection of the single workspace annotation camera.
            saved.annotation_scroll_points = panels.annotation_scroll_points;
            saved.vertical_content_origin_points = panel.vertical_content_origin_points;
            saved.read_stack_scroll_points = panel.read_stack_scroll_points;
            saved.read_panel_states = panel
                .read_panel_scrolls
                .iter()
                .map(|(track_id, state)| SessionReadPanelState {
                    track_id: *track_id,
                    scroll_points: state.scroll_points,
                    content_origin_points: state.content_origin_points,
                    latent_scroll_fraction: state.latent_scroll_fraction,
                    content_bottom_points: state.content_bottom_points,
                    density: state.density,
                })
                .collect();
            saved
        })
        .collect();
    (locus_of(panels.active()), saved, panels.active().id)
}

impl Viewer {
    /// Replaces an imported IGV session's placeholder locus (see
    /// [`ImportedLocusRequest`]); `None` for every other project.
    pub(crate) fn adopt_imported_locus(
        &mut self,
        request: Option<ImportedLocusRequest>,
    ) -> Result<()> {
        if let Some(request) = &request {
            self.panels.adopt_imported_locus(request)?;
            self.locus_editor = request.requested_locus().unwrap_or_default();
        }
        self.imported_locus = request;
        self.imported_feature_search = None;
        Ok(())
    }

    /// Finishes resolving a panel's requested locus against `dictionary`,
    /// queuing a feature search when it was an imported gene name.
    pub(crate) fn resolve_panel_requested_locus(
        &self,
        dictionary: &ReferenceDictionary,
        requested: Option<&str>,
    ) -> Result<RequestedLocus> {
        resolve_requested_locus(
            dictionary,
            &self.contig_aliases,
            requested,
            self.imported_locus
                .as_ref()
                .and_then(ImportedLocusRequest::feature_name),
        )
    }

    /// Called once panels have been placed on a newly opened dictionary:
    /// searches the annotations for an imported feature name, or says why
    /// the default locus is showing.
    pub(crate) fn finish_imported_locus(&mut self, feature_search: Option<String>) {
        if self.imported_locus.take().is_none() {
            return;
        }
        let Some(name) = feature_search else {
            return;
        };
        for panel in self.panels.iter_mut() {
            if panel.requested_locus.as_deref() == Some(name.as_str()) {
                panel.requested_locus = None;
            }
        }
        self.locus_editor.clone_from(&name);
        if self.track_engine.is_none() {
            self.report_warning(format!(
                "IGV session locus {name} is not a contig; showing the default locus until an \
                 annotation track is open to search for it"
            ));
        }
        self.imported_feature_search = Some(name);
        self.retry_imported_feature_search();
    }

    /// Runs the deferred search for an imported feature name as soon as an
    /// annotation engine exists (the built-in Genes track may be installed
    /// after the dictionary that placed the panels).
    pub(crate) fn retry_imported_feature_search(&mut self) {
        if self.track_engine.is_some()
            && let Some(name) = self.imported_feature_search.take()
        {
            self.request_annotation_search(&name, true, true);
        }
    }

    pub(crate) fn persist_settings(&mut self) {
        self.remember_current_locus(self.panels.active().id);
        // Serialising, fsyncing and renaming happen on the settings worker;
        // rapid saves collapse into one write of the newest snapshot.
        self.settings_writer.submit(&self.settings);
    }

    /// Surfaces a settings save that failed on the worker.
    pub(crate) fn poll_settings_writer(&mut self) {
        if let Some(error) = self.settings_writer.take_failure() {
            self.report_warning(format!(
                "Could not save settings to {}: {error}",
                self.settings_path.display()
            ));
        }
    }

    /// Blocks until every queued save is on disk; call last at exit.
    pub(crate) fn finish_settings_writer(&mut self) {
        self.settings_writer.finish();
    }

    pub(crate) fn handle_dropped_file(&mut self, path: &Path) {
        match classify_dropped_file(path) {
            DroppedFileKind::Alignment => self.open_alignment_source(path),
            DroppedFileKind::Reference => self.open_reference_source(path),
            DroppedFileKind::GenomeConfiguration => {
                if let Err(error) = self.open_genome_configuration(path) {
                    self.report_error(error.to_string());
                }
            }
            DroppedFileKind::Json => {
                // One read and one parse decide what the document is; it is then opened once.
                match std::fs::read_to_string(path).map(|text| classify_dropped_json(&text)) {
                    Ok(DroppedJsonKind::IgvGenome) => {
                        self.start_genome_load(path.to_string_lossy().into_owned());
                    }
                    Ok(DroppedJsonKind::GenomeConfiguration) => {
                        if let Err(error) = self.open_genome_configuration(path) {
                            self.report_error(error.to_string());
                        }
                    }
                    Ok(DroppedJsonKind::Session) => {
                        self.request_session_load(path.to_path_buf(), SessionLoadKind::Native);
                    }
                    Err(error) => {
                        self.report_error(format!("Could not open {}: {error}", path.display()));
                    }
                }
            }
            DroppedFileKind::IgvSession => {
                self.request_session_load(path.to_path_buf(), SessionLoadKind::IgvImport);
            }
            DroppedFileKind::Track => {
                if let Err(error) = self.add_track_source(path) {
                    self.report_error(error.to_string());
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
            .map(|_| ())
    }

    pub(crate) fn add_configured_track_source(
        &mut self,
        path: &Path,
        kind: TrackSpecKind,
        track_kind: TrackKind,
        name: String,
        format: Option<TrackFormat>,
    ) -> Result<TrackId> {
        let id = self.allocate_track_id();
        let mut config = TrackConfig::new(id, name, track_kind, track_source(path));
        config.format = format;
        self.add_preconfigured_track_source(path, kind, config)
    }

    pub(crate) fn add_preconfigured_track_source(
        &mut self,
        path: &Path,
        kind: TrackSpecKind,
        config: TrackConfig,
    ) -> Result<TrackId> {
        self.add_preconfigured_track_source_with_query_contig_aliases(
            path,
            kind,
            config,
            BTreeMap::new(),
        )
    }

    pub(crate) fn add_preconfigured_track_source_with_query_contig_aliases(
        &mut self,
        path: &Path,
        kind: TrackSpecKind,
        config: TrackConfig,
        query_contig_aliases: BTreeMap<String, String>,
    ) -> Result<TrackId> {
        let id = config.id;
        let specification = TrackSpec {
            id,
            name: config.name.clone(),
            path: path.to_path_buf(),
            kind,
        };
        if let Some(engine) = &self.track_engine {
            if query_contig_aliases.is_empty() {
                engine.try_add(specification)?;
            } else {
                engine.try_add_with_query_contig_aliases(specification, query_contig_aliases)?;
            }
        } else {
            // An empty alias map is the no-alias case.
            self.track_engine = Some(spawn_track_engine(vec![(
                specification,
                query_contig_aliases,
            )]));
        }
        let source_label = config.source.redacted_label();
        self.tracks.push(config);
        self.invalidate_navigation_contract("auxiliary track added");
        self.reconcile_track_presentation();
        self.synthetic = false;
        self.data_status = format!("Opening track {source_label}…");
        Ok(id)
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
            .peek_track_id()
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
        if let Some(largest) = expansion.children.iter().map(|track| track.id.get()).max() {
            self.reserve_track_ids_through(largest);
        }
        self.tracks.insert_after(parent_id, expansion.children);
        if selected_index > 0 {
            self.tracks
                .move_track(parent_id, parent_position.saturating_add(selected_index));
        }
        self.reconcile_track_presentation();
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

    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_session(&mut self, session: &SessionDocument) -> Result<()> {
        let mut rebound_session = session.clone();
        let landmark_error = LaunchOptions::rebind_builtin_session_tracks(
            &mut rebound_session,
            self.landmark_resource_root.as_deref(),
            self.landmark_catalogue.as_ref(),
        );
        let session = &rebound_session;
        let panel_count = session.panels.len().max(1);
        let panel_canvas_width = self.workspace_layout.projected_canvas_width(panel_count);
        let (panels, cytoband_path) = prepare_session_geometry(
            session,
            panel_canvas_width,
            self.canvas_geometry().viewport.top(),
        )?;
        self.ensure_engine_retirement_capacity(3)?;
        self.poll_alignment_retirement_tasks();
        anyhow::ensure!(
            self.alignment_retirement_tasks.len() <= rgv_core::MAXIMUM_ALIGNMENT_TRACKS,
            "Previous alignment sources are still closing; retry shortly"
        );
        let panel_ids = panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let controllers = self.prepare_alignment_track_controllers(
            session.alignment_tracks.clone(),
            session.reference.as_ref().map(source_path),
            session.remote_reference_index_url.clone(),
            &panel_ids,
        )?;
        let reference_engine = if let Some(source) = &session.reference {
            let engine = ReferenceEngine::spawn(8);
            if let Err(error) = engine.try_open_with_options(
                source_path(source),
                reference_open_options(session.remote_reference_index_url.clone()),
            ) {
                if let Some(worker) = engine.retire() {
                    self.engine_retirement_tasks.push(worker);
                }
                for controller in controllers {
                    self.retire_alignment_track_controller(controller);
                }
                return Err(error.into());
            }
            Some(engine)
        } else {
            None
        };
        let (specifications, tracks) = session_track_specs(session);
        let track_engine = (!specifications.is_empty())
            .then(|| spawn_track_engine_without_aliases(specifications));

        // Commit starts here. No fallible preparation or blocking destructor follows.
        if let Some(error) = landmark_error {
            self.landmark_catalogue_error = Some(error);
        }
        self.reference_open_error = None;
        self.automation_auxiliary_open_state.clear();
        self.unresolved_session_sources = session.unresolved_local_sources();
        self.session_migration_notices
            .clone_from(&session.migration_notices);
        self.promoted_read = None;
        self.read_name_editor.clear();
        self.variant_sample_search.clear();
        self.variant_settings_sample_search.clear();
        let alignment = primary_alignment_track(session).cloned();
        self.apply_primary_alignment_config(alignment.as_ref(), session.maximum_alignments)
            .expect("session filters were validated during preparation");
        self.track_presentation = session.presentation.clone();
        if self.track_presentation.pane_tracks.is_empty()
            && (!session.alignment_tracks.is_empty() || !session.tracks.is_empty())
        {
            self.track_presentation =
                WorkspacePresentation::from_legacy(&session.alignment_tracks, &session.tracks);
        }
        self.reference_source.clone_from(&session.reference);
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
        self.cytoband_path = cytoband_path;
        self.contig_aliases.clone_from(&session.contig_aliases);
        self.suppressed_builtin_gene_bundles
            .clone_from(&session.suppressed_builtin_gene_bundles);
        self.tracks = tracks;
        self.automation_auxiliary_open_state.extend(
            self.tracks
                .tracks()
                .iter()
                .map(|track| (track.id, AutomationAuxiliaryOpenState::Pending)),
        );
        self.retire_track_engine();
        self.track_engine = track_engine;
        self.panels = panels;
        // Panel identity replacement and its geometry must be one state
        // transition. Context refresh and display rebuilding call
        // `active_panel_rect`, so they may not observe the previous session's
        // panel list in the committed layout.
        self.synchronize_panel_widths();
        self.reference_dictionary = None;
        self.reference_revision = None;
        self.retire_reference_engine();
        self.reference_engine = reference_engine;
        self.refresh_active_panel_context();
        self.install_alignment_track_controllers(controllers);
        if alignment.is_some() {
            "Opening alignment tracks…".clone_into(&mut self.data_status);
        } else {
            self.panels.active_mut().pending_queries.alignment = false;
            self.synthetic = false;
            self.panels.active_mut().display_list = DisplayList::default();
            self.rebuild_all_panel_displays();
            self.request_all_panel_queries();
            self.mark_display_dirty("explicit display invalidation");
        }
        self.current_session_path = None;
        self.replace_document_identity(false);
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
        // The editor may contain an unapplied or invalid draft. The canonical
        // configuration already contains the last successfully applied clauses.
        filters
    }

    pub(crate) fn session_document(&self) -> SessionDocument {
        let (locus, panels, active_panel) = session_panel_documents(&self.panels);
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
            migration_notices: Vec::new(),
            locus,
            panels,
            active_panel,
            alignment_tracks,
            presentation: self.workspace_presentation_snapshot(),
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
            landmark_context: self
                .landmark_resolution
                .as_ref()
                .filter(|resolution| {
                    resolution.status == rgv_core::AssemblyResolutionStatus::Matched
                })
                .and_then(|resolution| resolution.bundle_id.as_ref())
                .and_then(|bundle_id| {
                    self.landmark_catalogue
                        .as_ref()?
                        .bundle(bundle_id)
                        .map(|bundle| rgv_core::BuiltInLandmarkContext {
                            bundle_id: bundle.id.clone(),
                            bundle_version: bundle.version.clone(),
                        })
                }),
            suppressed_builtin_gene_bundles: self.suppressed_builtin_gene_bundles.clone(),
            tracks: self.tracks.tracks().to_vec(),
            maximum_alignments: self.workspace_alignment_limit,
        }
    }
}

#[cfg(test)]
mod preparation_tests {
    use super::*;

    #[test]
    fn unsupported_cytobands_fail_before_any_replacement_is_constructed() {
        let a = SessionDocument::new(SessionLocus {
            contig: "chr1".into(),
            start: 0,
            end: 100,
        });
        let mut b = a.clone();
        b.reference = Some(TrackSource::Local {
            path: "different.fa".into(),
        });
        b.genome_name = Some("different assembly".into());
        b.cytobands = Some(TrackSource::Remote {
            url: "https://example.invalid/bands.txt".into(),
        });
        let (panels_a, _) = prepare_session_geometry(&a, 800.0, 0.0).unwrap();
        assert!(prepare_session_geometry(&b, 800.0, 0.0).is_err());
        assert_eq!(panels_a.active().contig_name, "chr1");
        assert_eq!(panels_a.active().viewport.interval().start(), a.locus.start);
        b.cytobands = None;
        assert!(
            prepare_session_geometry(&b, 800.0, 0.0).is_ok(),
            "missing sources are a recoverable per-source state"
        );
        b.panels[0].locus.end = 0;
        assert!(prepare_session_geometry(&b, 800.0, 0.0).is_err());
    }
}
