use super::super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameTargetPlan {
    render_surface: bool,
    render_capture: bool,
}

const fn frame_target_plan(capture_requested: bool) -> FrameTargetPlan {
    FrameTargetPlan {
        // A capture is an additional output, never a replacement for the
        // presented window frame. This keeps navigation commit and visible
        // presentation in the same queue submission.
        render_surface: true,
        render_capture: capture_requested,
    }
}

/// Builds the alignment-track configuration used at launch.
///
/// A track restored from a session remains wholly session-owned. A track synthesized from the
/// command-line BAM/CRAM, however, must inherit the command-line complete-read limit instead of
/// retaining `SessionAlignmentTrack::new`'s legacy 50,000-read default.
fn alignment_track_configs_for_launch(
    mut configured_tracks: Vec<SessionAlignmentTrack>,
    alignment_source: Option<TrackSource>,
    next_track_id: TrackId,
    remote_index_url: Option<String>,
    cli_maximum_alignments: usize,
) -> Vec<SessionAlignmentTrack> {
    if configured_tracks.is_empty()
        && let Some(source) = alignment_source
    {
        let mut config = SessionAlignmentTrack::new(
            next_track_id,
            source_display_name(Some(&source), "Alignments"),
            source,
        );
        config.remote_index_url = remote_index_url;
        config.maximum_alignments = cli_maximum_alignments.max(1);
        configured_tracks.push(config);
    }
    configured_tracks
}

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn new(
        window: Arc<Window>,
        options: LaunchOptions,
        process_started: Instant,
    ) -> Result<Self> {
        let application_instance_id = uuid::Uuid::new_v4().to_string();
        let size = window.inner_size();
        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(Arc::clone(&window))
            .context("failed to create GPU surface")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .context("no compatible GPU adapter was found")?;
        let adapter_info = adapter.get_info();
        tracing::info!(
            name = adapter_info.name,
            backend = ?adapter_info.backend,
            "selected GPU adapter"
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("RGV GPU device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
            .context("failed to create GPU device")?;
        let mut surface_config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .context("GPU surface has no supported configuration")?;
        surface_config.present_mode = wgpu::PresentMode::AutoVsync;
        surface_config.desired_maximum_frame_latency = 2;
        surface.configure(&device, &surface_config);

        let rectangle_renderer = RectangleRenderer::new(&device, surface_config.format);
        let context_scene_gpu_renderer =
            ContextSceneGpuRenderer::new(&device, surface_config.format);
        let alignment_gpu_renderer = AlignmentGpuRenderer::new(&device, surface_config.format);
        let alignment_summary_gpu_renderer =
            AlignmentSummaryGpuRenderer::new(&device, surface_config.format);
        let egui_context = egui::Context::default();
        egui_context.set_visuals(egui::Visuals::light());
        let egui_state = egui_winit::State::new(
            egui_context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let egui_renderer =
            egui_wgpu::Renderer::new(&device, surface_config.format, RendererOptions::default());
        let logical_size = size.to_logical::<f64>(window.scale_factor());
        let initial_properties_width = 340.0_f32.min((logical_size.width as f32 - 320.0).max(30.0));
        let workspace_bounds = egui::Rect::from_min_max(
            egui::pos2(0.0, MENU_HEIGHT + NAVIGATION_HEIGHT),
            egui::pos2(
                (logical_size.width as f32 - initial_properties_width).max(1.0),
                (logical_size.height as f32 - STATUS_HEIGHT)
                    .max(MENU_HEIGHT + NAVIGATION_HEIGHT + 1.0),
            ),
        );
        let shell_bottom =
            (logical_size.height as f32 - STATUS_HEIGHT).max(MENU_HEIGHT + NAVIGATION_HEIGHT + 1.0);
        let initial_properties = egui::Rect::from_min_max(
            egui::pos2(workspace_bounds.right(), MENU_HEIGHT + NAVIGATION_HEIGHT),
            egui::pos2(logical_size.width as f32, shell_bottom),
        );
        let bootstrap_layout = WorkspaceLayoutSnapshot::compute(
            WorkspaceLayoutInput::new(
                workspace_bounds,
                initial_properties,
                true,
                vec![WorkspacePanelLayoutInput {
                    panel_id: INITIAL_PANEL_ID,
                    contig_name: "chrSynthetic".to_owned(),
                    contig_length: SYNTHETIC_CONTIG_LENGTH,
                    cytobands: CytobandAvailability::Absent,
                }],
                GenomeContextMetrics::default(),
                window.scale_factor() as f32,
                egui_context.zoom_factor(),
            ),
            0,
        );
        let canvas_geometry = bootstrap_layout.canvas_geometry();
        let mut settings = options.loaded_settings.clone();
        apply_accessibility_preferences(&egui_context, &settings);
        if let Some(path) = options.session.as_ref().or(options.import_igv.as_ref()) {
            settings.remember_session(path.display().to_string());
        }
        let total_cache_bytes = settings.cache_budget_mib.saturating_mul(1024 * 1024).max(1);
        // One enforcing authority is shared by every alignment source, immutable scene layer, and
        // derived GPU page. The explicit transition reserve permits one old and one candidate
        // visible set; transient decoder/upload work is reported under its own equal ceiling.
        let alignment_scene_ledger = AllocationLedger::new_with_transient_budget(
            total_cache_bytes,
            total_cache_bytes,
            total_cache_bytes,
        );
        let alignment_ledger_progress = alignment_scene_ledger.subscribe_progress();
        let context_lane_gpu_budget_bytes = total_cache_bytes;
        let (track_specifications, tracks) = build_track_specs(&options);
        let contig_aliases = launch_contig_aliases(&options)?;
        let session_workspace = options.loaded_session.is_some();
        let track_only = options.bam.is_none()
            && (options.reference.is_some()
                || !track_specifications.is_empty()
                || session_workspace);
        let synthetic = options.bam.is_none()
            && options.reference.is_none()
            && track_specifications.is_empty()
            && !session_workspace;
        let (initial_interval, initial_contig_name, initial_contig_length) = if track_only {
            standalone_locus(options.locus.as_deref().unwrap_or("chr1:1-100000"))?
        } else {
            (
                GenomicInterval::new(
                    ContigId::new(0),
                    INITIAL_INTERVAL_START,
                    INITIAL_INTERVAL_END,
                )?,
                "chrSynthetic".to_owned(),
                SYNTHETIC_CONTIG_LENGTH,
            )
        };
        let viewport = Viewport::new(
            initial_interval,
            f64::from(canvas_geometry.genomic.width().max(1.0)),
        )?;
        let initial_dictionary = ReferenceDictionary::from_contigs([(
            initial_contig_name.clone(),
            initial_contig_length,
        )]);
        let assembly_identity = options
            .loaded_genome
            .as_ref()
            .and_then(|genome| genome.assembly_identity.clone())
            .or_else(|| {
                options
                    .loaded_session
                    .as_ref()
                    .and_then(|session| session.assembly_identity.clone())
            });
        let configured_annotation_count = options
            .loaded_genome
            .as_ref()
            .map_or(0, |genome| genome.default_annotations.len());
        let mut assembly = load_assembly(
            &options.genome_name,
            initial_dictionary,
            options.cytobands.as_deref(),
        );
        assembly.identity.clone_from(&assembly_identity);
        assembly.default_annotation_count = configured_annotation_count;
        let mut panel = ViewPanelController::new(
            INITIAL_PANEL_ID,
            viewport,
            options.locus.clone(),
            initial_contig_name,
            initial_contig_length,
        );
        panel.content_bottom_points = canvas_geometry.viewport.top();
        let mut panels = options.loaded_session.as_ref().map_or_else(
            || Ok(PanelWorkspace::new(panel)),
            |session| {
                let panel_count = session.panels.len().max(1);
                let panel_canvas_width = bootstrap_layout.projected_canvas_width(panel_count);
                PanelWorkspace::from_session(
                    session,
                    panel_canvas_width,
                    canvas_geometry.viewport.top(),
                )
            },
        )?;
        let initial_panel_inputs = panels
            .iter()
            .map(|panel| {
                let cytobands = if assembly
                    .cytobands_for(panel.viewport.interval().contig())
                    .next()
                    .is_some()
                {
                    CytobandAvailability::Present
                } else {
                    CytobandAvailability::Absent
                };
                WorkspacePanelLayoutInput {
                    panel_id: panel.id,
                    contig_name: panel.contig_name.clone(),
                    contig_length: panel.contig_length,
                    cytobands,
                }
            })
            .collect::<Vec<_>>();
        let workspace_layout = WorkspaceLayoutSnapshot::compute(
            WorkspaceLayoutInput::new(
                workspace_bounds,
                initial_properties,
                true,
                initial_panel_inputs,
                GenomeContextMetrics::default(),
                window.scale_factor() as f32,
                egui_context.zoom_factor(),
            ),
            1,
        );
        let canvas_geometry = workspace_layout.canvas_geometry();
        for (panel, layout) in panels.iter_mut().zip(&workspace_layout.panels) {
            if let Ok(viewport) = panel
                .viewport
                .resized(f64::from(layout.canvas.width().max(1.0)))
            {
                panel.viewport = viewport;
            }
            panel.content_bottom_points = canvas_geometry.viewport.top();
        }
        let active_interval = panels.active().viewport.interval();
        let locus_editor = format!(
            "{}:{}-{}",
            panels.active().contig_name,
            active_interval.start().saturating_add(1),
            active_interval.end()
        );
        let panel_ids = panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let track_engine =
            (!track_specifications.is_empty()).then(|| TrackEngine::spawn(track_specifications, 8));
        let automation_auxiliary_open_state = tracks
            .tracks()
            .iter()
            .map(|track| (track.id, AutomationAuxiliaryOpenState::Pending))
            .collect();
        let alignment_source = options.bam.as_ref().map(|path| track_source(path));
        let configured_alignment_tracks = options
            .loaded_session
            .as_ref()
            .map_or_else(Vec::new, |session| session.alignment_tracks.clone());
        let next_alignment_track_id = tracks
            .tracks()
            .iter()
            .map(|track| track.id.get())
            .chain(
                configured_alignment_tracks
                    .iter()
                    .map(|track| track.id.get()),
            )
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        let alignment_track_configs = alignment_track_configs_for_launch(
            configured_alignment_tracks,
            alignment_source.clone(),
            TrackId::new(next_alignment_track_id),
            options.remote_index_url.clone(),
            options.max_alignments,
        );
        let mut track_presentation = options.loaded_session.as_ref().map_or_else(
            || WorkspacePresentation::from_legacy(&alignment_track_configs, tracks.tracks()),
            |session| session.presentation.clone(),
        );
        if track_presentation.pane_tracks.is_empty()
            && (!alignment_track_configs.is_empty() || !tracks.tracks().is_empty())
        {
            track_presentation =
                WorkspacePresentation::from_legacy(&alignment_track_configs, tracks.tracks());
        }
        if synthetic {
            track_presentation.pane_tracks.extend([
                SessionPaneTrackState {
                    channel: ui::TrackPane::Annotation,
                    track_id: SYNTHETIC_ALIGNMENT_TRACK_ID,
                    content_visible: true,
                    preferred_height: None,
                },
                SessionPaneTrackState {
                    channel: ui::TrackPane::Reads,
                    track_id: SYNTHETIC_ALIGNMENT_TRACK_ID,
                    content_visible: true,
                    preferred_height: None,
                },
            ]);
        }
        let primary_alignment = alignment_track_configs.first();
        let alignment_name = if synthetic {
            "Synthetic reads".to_owned()
        } else {
            primary_alignment.map_or_else(
                || source_display_name(alignment_source.as_ref(), "Alignments"),
                |track| track.name.clone(),
            )
        };
        let reference_source = options.reference.as_ref().map(|path| track_source(path));
        let remote_reference_index_url = options.loaded_session.as_ref().map_or_else(
            || options.remote_reference_index_url.clone(),
            |session| session.remote_reference_index_url.clone(),
        );
        let reference_engine = if let Some(source) = &reference_source {
            let engine = ReferenceEngine::spawn(8);
            engine.try_open_with_options(
                source_path(source),
                reference_open_options(remote_reference_index_url.clone()),
            )?;
            Some(engine)
        } else {
            None
        };
        let (filter_editor, active_filters) = initial_filters(options.loaded_session.as_ref())?;
        let session_filters = options
            .loaded_session
            .as_ref()
            .and_then(primary_alignment_track)
            .map_or_else(SessionFilters::default, |track| track.filters.clone());
        let alignment_visible = primary_alignment.is_none_or(|track| track.visible);
        let alignment_collapsed = primary_alignment.is_some_and(|track| track.collapsed);
        let workspace_alignment_limit = options
            .loaded_session
            .as_ref()
            .map_or(options.max_alignments, |session| session.maximum_alignments)
            .max(1);
        let style_tag_editor = persisted_style_tag(
            session_filters.alignment_color_mode,
            session_filters.alignment_group_mode,
            session_filters.alignment_sort_mode,
        );
        let alignment_scene_budget_bytes =
            NonZeroUsize::new(total_cache_bytes).expect("scene budget is non-zero");
        let mut alignment_tracks = Vec::with_capacity(alignment_track_configs.len());
        for config in alignment_track_configs {
            alignment_tracks.push(spawn_alignment_track_controller(
                config,
                alignment_scene_budget_bytes,
                alignment_scene_ledger.clone(),
                options.reference.clone(),
                remote_reference_index_url.clone(),
                &panel_ids,
            )?);
        }
        let scene_track_ids = alignment_tracks
            .iter()
            .filter(|track| track.scene_engine.is_some())
            .map(|track| track.config.id)
            .collect::<Vec<_>>();
        let alignment_panel_scene_stores = panel_ids
            .iter()
            .copied()
            .map(|panel_id| {
                (
                    panel_id,
                    AlignmentPanelSceneStore::new(scene_track_ids.iter().copied()),
                )
            })
            .collect();
        let mut data_status = if !alignment_tracks.is_empty() {
            "Opening alignment source…".to_owned()
        } else if reference_engine.is_some() {
            "Opening reference genome…".to_owned()
        } else if track_only {
            "Opening genomic tracks…".to_owned()
        } else {
            "Synthetic GPU workload".to_owned()
        };
        if let (Some(path), Some(session)) = (&options.session, &options.loaded_session) {
            let unresolved = session.unresolved_local_sources();
            if !unresolved.is_empty() {
                data_status = opened_project_status(path, &unresolved);
            }
        }
        if options.recovered_unclean_exit {
            let recovery = if options.recovery_candidates.is_empty() {
                "no recovery snapshot was found"
            } else {
                "choose a recovery snapshot from the recovery window"
            };
            data_status =
                format!("The previous RGV run ended unexpectedly; {recovery} · {data_status}");
        }
        let next_autosave =
            Instant::now() + Duration::from_secs(settings.autosave_interval_seconds);
        let recovery_open =
            options.recovered_unclean_exit && !options.recovery_candidates.is_empty();
        let initial_open_panel = options
            .loaded_igv_report
            .as_ref()
            .map(|_| UiPanel::IgvImportReport);
        let automation_report_path = options.automation_report.clone().or_else(|| {
            options
                .batch
                .as_ref()
                .map(|path| path.with_extension("report.json"))
        });
        let automation_environment = AutomationEnvironmentTelemetry {
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            gpu_adapter: adapter_info.name.clone(),
            gpu_backend: format!("{:?}", adapter_info.backend),
            surface_width_pixels: surface_config.width,
            surface_height_pixels: surface_config.height,
            scale_factor: window.scale_factor(),
        };
        let automation = options.loaded_batch.map(|batch| {
            AutomationRunner::new_with_telemetry(
                batch,
                automation_report_path.unwrap_or_else(|| PathBuf::from("rgv-batch-report.json")),
                Some(process_started),
                automation_environment,
            )
        });
        let control_service = if let Some(port) = options.control_port {
            let root = options
                .control_root
                .clone()
                .context("local control has no selected filesystem root")?;
            let secret = options
                .control_secret
                .clone()
                .context("local control has no loaded session secret")?;
            let service = ControlService::start(
                port,
                root.clone(),
                secret,
                ControlStatusMetadata {
                    application_instance_id: application_instance_id.clone(),
                    renderer_backend: format!("{:?}", adapter_info.backend),
                    surface_width_pixels: surface_config.width,
                    surface_height_pixels: surface_config.height,
                    scale_factor: window.scale_factor(),
                },
            )
            .map_err(anyhow::Error::msg)?;
            tracing::info!(
                address = %service.address(),
                root = %root.display(),
                "authenticated local control enabled"
            );
            Some(service)
        } else {
            None
        };
        let initial_alignment_scene_budget = AlignmentSceneBudgetSnapshot::from_progress(
            alignment_scene_ledger.progress().map_err(|error| {
                anyhow::anyhow!("could not read initial alignment scene budget: {error}")
            })?,
            0,
        );
        Ok(Self {
            application_instance_id,
            workspace_revision: 1,
            last_saved_workspace_revision: 1,
            next_evidence_revision: 1,
            automation_alignment_handles: HashMap::new(),
            window,
            surface,
            device,
            queue,
            surface_config,
            rectangle_renderer,
            context_scene_gpu_renderer,
            context_scene_gpu_frame: ContextSceneGpuFrame::default(),
            alignment_scene_ledger,
            alignment_ledger_progress,
            alignment_gpu_renderer,
            alignment_summary_gpu_renderer,
            alignment_presented_epoch: None,
            alignment_gpu_residency_bundle: None,
            alignment_gpu_prefetch: AlignmentGpuPrefetchState::default(),
            alignment_gpu_publication_retry: None,
            alignment_gpu_speculation_gate: AlignmentGpuSpeculationGate::default(),
            alignment_panel_scene_stores,
            alignment_scene_owner_clock: 0,
            alignment_scene_request_clock: 0,
            alignment_scene_publication_authority: AlignmentScenePublicationAuthority::default(),
            navigation_stale_result_rejections: 0,
            egui_context,
            egui_state,
            egui_renderer,
            panels,
            workspace_layout,
            genomic_canvas_layer_id: None,
            pane_divider_rect: None,
            composite_display_list: DisplayList::default(),
            shared_annotation_frame_heights: BTreeMap::new(),
            alignment_tracks,
            empty_alignment: EmptyAlignmentState {
                filters: session_filters.clone(),
                filter_editor: filter_editor.clone(),
                active_filters: active_filters.clone(),
                name: alignment_name.clone(),
                visible: alignment_visible,
                collapsed: alignment_collapsed,
                maximum_alignments: workspace_alignment_limit,
            },
            alignment_retirement_tasks: Vec::new(),
            reference_engine,
            reference_revision: None,
            reference_open_error: None,
            track_engine,
            automation_auxiliary_open_state,
            tracks,
            track_presentation,
            reference_source,
            remote_reference_index_url,
            reference_remote_io_stats: None,
            reference_dictionary: None,
            contig_aliases,
            assembly,
            assembly_identity,
            suggested_assembly_identity: None,
            landmark_resource_root: options.landmark_resource_root.clone(),
            landmark_catalogue: options.loaded_landmark_catalogue.clone(),
            landmark_catalogue_error: options.landmark_catalogue_error.clone(),
            landmark_resolution: None,
            landmark_search_index: None,
            suppressed_builtin_gene_bundles: options
                .loaded_session
                .as_ref()
                .map_or_else(BTreeSet::new, |session| {
                    session.suppressed_builtin_gene_bundles.clone()
                }),
            assembly_name: options.genome_name,
            cytoband_path: options.cytobands,
            data_status,
            announced_status: String::new(),
            reference_reverse_complement: options
                .loaded_session
                .as_ref()
                .is_some_and(|session| session.reference_reverse_complement),
            reference_translation_frames: options
                .loaded_session
                .as_ref()
                .map_or_else(Vec::new, |session| {
                    session.reference_translation_frames.clone()
                }),
            reference_genetic_code: options
                .loaded_session
                .as_ref()
                .map_or_else(GeneticCode::default, |session| {
                    session.reference_genetic_code
                }),
            regions_of_interest: options
                .loaded_session
                .as_ref()
                .map_or_else(Vec::new, |session| session.regions_of_interest.clone()),
            review_items: options
                .loaded_session
                .as_ref()
                .map_or_else(Vec::new, |session| session.review_items.clone()),
            sample_metadata: options
                .loaded_session
                .as_ref()
                .and_then(|session| session.sample_metadata.clone()),
            metadata_filter_column: 0,
            metadata_filter_value: None,
            metadata_selected_tracks: BTreeSet::new(),
            metadata_bulk_height: 120.0,
            metadata_remove_armed: false,
            review_status_filter: None,
            style_tag_editor,
            read_name_editor: String::new(),
            variant_sample_search: String::new(),
            variant_settings_sample_search: String::new(),
            variant_predicate_scope: VariantFieldScope::Info,
            variant_predicate_key: "AF".to_owned(),
            variant_predicate_operator: VariantPredicateOperator::Exists,
            variant_predicate_value: String::new(),
            variant_predicate_missing: VariantMissingValueRule::Exclude,
            modification_code_cache: None,
            export_tasks: Vec::new(),
            pending_native_window_capture: None,
            session_save_tasks: Vec::new(),
            session_load_tasks: Vec::new(),
            workspace_list_import_tasks: Vec::new(),
            promoted_read: None,
            synthetic,
            workspace_alignment_limit,
            navigation_contract_revision: 0,
            display_dirty: true,
            display_upload_pending_metrics: false,
            pending_resolution_query_refresh: None,
            automation_properties_resize_active: false,
            properties_resize_gesture_active: false,
            workspace_layout_recomputations: 0,
            resize_query_refreshes: 0,
            dragging: false,
            drag_origin_interval: None,
            cursor: PhysicalPosition::new(0.0, 0.0),
            last_cursor: None,
            press_cursor: None,
            smoothed_frame_ms: 16.7,
            next_egui_repaint: None,
            modifiers: ModifiersState::empty(),
            pending_command: None,
            open_panel: initial_open_panel,
            remote_source_editor: RemoteSourceEditor::default(),
            diagnostics_open: false,
            interaction_trace: InteractionTraceRecorder::default(),
            sashimi_interaction_diagnostics: SashimiInteractionDiagnostics::default(),
            command_search: String::new(),
            annotation_search_generation: 0,
            annotation_navigation_generation: 0,
            annotation_search_results: TrackSearchResults::default(),
            pending_annotation_navigation: None,
            settings,
            settings_path: options.settings_path.unwrap_or_else(default_settings_path),
            current_session_path: options.session,
            unresolved_session_sources: options
                .loaded_session
                .as_ref()
                .map_or_else(Vec::new, SessionDocument::unresolved_local_sources),
            session_migration_notices: options
                .loaded_session
                .as_ref()
                .map_or_else(Vec::new, |session| session.migration_notices.clone()),
            igv_import_report: options.loaded_igv_report,
            next_autosave,
            recovery_candidates: options.recovery_candidates,
            recovery_open,
            automation,
            control_service,
            automation_exit_requested: false,
            exit_confirmation_open: false,
            user_exit_requested: false,
            context_lane_gpu_budget_bytes,
            alignment_scene_budget: initial_alignment_scene_budget,
            locus_editor,
            resizing_annotation_track: None,
            resizing_read_track: None,
            resizing_read_pane: None,
            active_stack_scrollbar_gesture: None,
            horizontal_overview_drag: None,
            vertical_overview_drag: None,
            overview_capture: None,
            captured_motion: (0.0, 0.0),
            captured_motion_distance: 0.0,
            suppress_horizontal_overview_click: false,
        })
    }

    pub(crate) fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        self.reconfigure_surface();
        // The next egui frame owns the final shell rectangle, including its
        // persisted Properties width. Keep the currently displayed geometry
        // committed until that frame can replace the UI and genomic display
        // list together.
        self.display_dirty = true;
        self.window.request_redraw();
    }

    pub(crate) fn reconfigure_surface(&self) {
        if self.surface_config.width > 0 && self.surface_config.height > 0 {
            self.surface.configure(&self.device, &self.surface_config);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_composed_frame_target(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        paint_jobs: &[egui::ClippedPrimitive],
        screen_descriptor: &ScreenDescriptor,
        scale_factor: f32,
    ) {
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("RGV genomic canvas"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 1.0,
                            g: 1.0,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let scissor = self.workspace_layout.physical_genomic_scissor(
                scale_factor,
                self.surface_config.width,
                self.surface_config.height,
            );
            if scissor.width > 0 && scissor.height > 0 {
                pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
                self.rectangle_renderer.render(&mut pass);
                self.context_scene_gpu_renderer
                    .render(&mut pass, &self.context_scene_gpu_frame);
            }
            if let Some(bundle) = self
                .alignment_presented_epoch
                .as_ref()
                .and_then(|epoch| epoch.gpu_bundle.as_ref())
                && alignment_gpu_bundle_is_current(
                    bundle,
                    &self.alignment_scene_publication_authority,
                )
            {
                if let Some(frame) = &bundle.summaries
                    && let Err(error) = self.alignment_summary_gpu_renderer.render(&mut pass, frame)
                {
                    tracing::error!(
                        %error,
                        "could not render unified alignment summary frame"
                    );
                }
                if let Some(frame) = &bundle.reads
                    && let Err(error) = self.alignment_gpu_renderer.render(&mut pass, frame)
                {
                    tracing::error!(%error, "could not render unified alignment scene frame");
                }
            }
        }
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("RGV application chrome"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.egui_renderer
                .render(&mut pass.forget_lifetime(), paint_jobs, screen_descriptor);
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn submit_ui_frame(
        &mut self,
        full_output: egui::FullOutput,
        prepared_stats: Option<PrepareStats>,
        frame_started: Instant,
        repaint_delay: Duration,
    ) {
        let tessellation_started = Instant::now();
        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);
        let paint_jobs = self
            .egui_context
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        if self.interaction_trace.is_active() {
            self.interaction_trace.record_phase(
                InteractionTraceStage::EguiTessellationFinished,
                tessellation_started.elapsed(),
            );
        }
        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point: full_output.pixels_per_point,
        };
        for (id, image_delta) in &full_output.textures_delta.set {
            self.egui_renderer
                .update_texture(&self.device, &self.queue, *id, image_delta);
        }

        let capture_task = self.pending_native_window_capture.take().map(|mut task| {
            // A Committing frame renders the authoritative target and becomes
            // settled immediately after this submission, so its pixels are not
            // labelled as a still-active transaction.
            task.automation_data["navigation_transaction_active"] =
                serde_json::Value::Bool(self.panels.iter().any(|panel| {
                    panel.navigation_presentation.is_unsettled()
                        && panel.navigation_presentation.phase
                            != NavigationPresentationPhase::Committing
                }));
            task
        });
        let capture_texture = capture_task.as_ref().map(|_| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("RGV native window capture"),
                size: wgpu::Extent3d {
                    width: self.surface_config.width,
                    height: self.surface_config.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.surface_config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        });
        let surface_started = Instant::now();
        let surface_frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Some(frame),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.reconfigure_surface();
                Some(frame)
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                self.pending_native_window_capture = capture_task;
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                if self.automation.is_none() {
                    self.pending_native_window_capture = capture_task;
                    return;
                }
                // Native automation may be fully covered by the controlling
                // application. Render the same authoritative frame to a
                // private target so transactional navigation and deterministic
                // GPU captures can progress without stealing focus.
                None
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.pending_native_window_capture = capture_task;
                self.reconfigure_surface();
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.pending_native_window_capture = capture_task;
                self.reconfigure_surface();
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.pending_native_window_capture = capture_task;
                tracing::warn!("surface validation failed while acquiring a frame");
                self.window.request_redraw();
                return;
            }
        };
        let automation_surface_texture = surface_frame.is_none().then(|| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("RGV occluded automation frame"),
                size: wgpu::Extent3d {
                    width: self.surface_config.width,
                    height: self.surface_config.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.surface_config.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        });
        let surface_view = surface_frame
            .as_ref()
            .map(|frame| &frame.texture)
            .or(automation_surface_texture.as_ref())
            .expect("presented or automation frame target exists")
            .create_view(&wgpu::TextureViewDescriptor::default());
        if self.interaction_trace.is_active() && automation_surface_texture.is_none() {
            self.interaction_trace.record_phase(
                InteractionTraceStage::SurfaceAcquired,
                surface_started.elapsed(),
            );
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("RGV frame encoder"),
            });
        let egui_buffers_started = Instant::now();
        let callback_commands = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        if self.interaction_trace.is_active() {
            self.interaction_trace.record_phase(
                InteractionTraceStage::EguiBuffersFinished,
                egui_buffers_started.elapsed(),
            );
        }
        let scale_factor = self.window.scale_factor() as f32;
        let targets = frame_target_plan(capture_texture.is_some());
        if targets.render_surface {
            self.render_composed_frame_target(
                &mut encoder,
                &surface_view,
                &paint_jobs,
                &screen_descriptor,
                scale_factor,
            );
        }
        if targets.render_capture
            && let Some(texture) = capture_texture.as_ref()
        {
            let capture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.render_composed_frame_target(
                &mut encoder,
                &capture_view,
                &paint_jobs,
                &screen_descriptor,
                scale_factor,
            );
        }

        let capture_readback = capture_texture.as_ref().map(|texture| {
            let unpadded_bytes_per_row = self.surface_config.width * 4;
            let padded_bytes_per_row = unpadded_bytes_per_row
                .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("RGV native window capture readback"),
                size: u64::from(padded_bytes_per_row) * u64::from(self.surface_config.height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bytes_per_row),
                        rows_per_image: Some(self.surface_config.height),
                    },
                },
                wgpu::Extent3d {
                    width: self.surface_config.width,
                    height: self.surface_config.height,
                    depth_or_array_layers: 1,
                },
            );
            (buffer, padded_bytes_per_row)
        });

        let queue_started = Instant::now();
        self.queue
            .submit(callback_commands.into_iter().chain([encoder.finish()]));
        let navigation_committed = self.finish_navigation_commits();
        if navigation_committed {
            // The submitted display was composed while the presentation was
            // Committing, when semantic hit targets are intentionally
            // suppressed. Republish those targets from the same settled
            // display cache now that the frame is authoritative. Without this
            // handoff, visible sashimi curves remain non-interactive until an
            // unrelated layout or scroll rebuild happens.
            if !self.refresh_active_panel_scroll_display() {
                self.display_dirty = true;
                self.window.request_redraw();
            }
        }
        if self.interaction_trace.is_active() {
            self.interaction_trace.record_phase(
                InteractionTraceStage::QueueSubmitted,
                queue_started.elapsed(),
            );
        }
        let present_started = Instant::now();
        if let Some(surface_frame) = surface_frame {
            surface_frame.present();
        }
        if self.interaction_trace.is_active() && automation_surface_texture.is_none() {
            self.interaction_trace.record_phase(
                InteractionTraceStage::PresentCalled,
                present_started.elapsed(),
            );
        }
        if let (Some(task), Some((buffer, padded_bytes_per_row))) = (capture_task, capture_readback)
        {
            let pixels = self.read_native_window_capture(&buffer, padded_bytes_per_row);
            self.complete_native_window_capture(task, pixels);
        }
        let frame_ms = frame_started.elapsed().as_secs_f64() * 1_000.0;
        self.interaction_trace
            .frame_finished(frame_started.elapsed());
        self.smoothed_frame_ms = self.smoothed_frame_ms * 0.9 + frame_ms * 0.1;
        let navigation_frame = self
            .automation
            .as_ref()
            .and_then(AutomationRunner::pending_navigation_target)
            .and_then(|(panel_id, target)| {
                let panel = self.panels.get(panel_id)?;
                (panel.viewport.interval() == target).then(|| {
                    let authoritative = panel.navigation_presentation.is_interactive()
                        && self.panel_navigation_data_is_settled(panel_id);
                    (authoritative, self.automation_navigation_state(panel_id))
                })
            });
        if let Some(runner) = self.automation.as_mut().filter(|runner| !runner.finished) {
            if let Some((authoritative, state)) = navigation_frame {
                runner.record_navigation_frame(
                    Instant::now(),
                    frame_ms,
                    prepared_stats,
                    authoritative,
                    &state,
                );
            }
            runner.record_frame(
                Instant::now(),
                frame_ms,
                prepared_stats,
                [self.surface_config.width, self.surface_config.height],
                self.window.scale_factor(),
            );
        }
        if repaint_delay.is_zero() {
            self.next_egui_repaint = None;
            self.window.request_redraw();
        } else if repaint_delay != Duration::MAX {
            self.next_egui_repaint = Some(Instant::now() + repaint_delay);
        } else {
            self.next_egui_repaint = None;
        }
        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
    }

    fn read_native_window_capture(
        &self,
        buffer: &wgpu::Buffer,
        padded_bytes_per_row: u32,
    ) -> std::result::Result<Vec<u8>, String> {
        let slice = buffer.slice(..);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|error| format!("GPU capture polling failed: {error}"))?;
        receiver
            .recv()
            .map_err(|_| "GPU capture mapping callback was lost".to_owned())?
            .map_err(|error| format!("GPU capture mapping failed: {error}"))?;
        let mapped = slice.get_mapped_range();
        let pixels = decoded_capture_pixels(
            &mapped,
            self.surface_config.width,
            self.surface_config.height,
            padded_bytes_per_row,
            self.surface_config.format,
        );
        drop(mapped);
        buffer.unmap();
        pixels
    }

    fn complete_native_window_capture(
        &mut self,
        task: NativeWindowCaptureTask,
        pixels: std::result::Result<Vec<u8>, String>,
    ) {
        let NativeWindowCaptureTask {
            path,
            automation_command,
            automation_data,
        } = task;
        let pixels = match pixels {
            Ok(pixels) => pixels,
            Err(message) => {
                self.finish_automation_command(
                    automation_command,
                    Err((AutomationErrorCode::Internal, message)),
                );
                return;
            }
        };
        let width = self.surface_config.width;
        let height = self.surface_config.height;
        let start = self.start_automation_export_job_with_data(
            automation_command,
            path,
            automation_data,
            move |path| {
                image::save_buffer_with_format(
                    path,
                    &pixels,
                    width,
                    height,
                    image::ColorType::Rgba8,
                    image::ImageFormat::Png,
                )
                .map_err(std::io::Error::other)
            },
        );
        if let Err(error) = start {
            self.finish_automation_command(
                automation_command,
                Err((AutomationErrorCode::Io, error.to_string())),
            );
        }
    }
}

fn decoded_capture_pixels(
    mapped: &[u8],
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
    format: wgpu::TextureFormat,
) -> std::result::Result<Vec<u8>, String> {
    let unpadded_bytes_per_row = width as usize * 4;
    let mut pixels = Vec::with_capacity(unpadded_bytes_per_row * height as usize);
    for row in mapped
        .chunks_exact(padded_bytes_per_row as usize)
        .take(height as usize)
    {
        pixels.extend_from_slice(&row[..unpadded_bytes_per_row]);
    }
    match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => {}
        format => {
            return Err(format!(
                "unsupported native capture texture format {format:?}"
            ));
        }
    }
    Ok(pixels)
}

#[cfg(test)]
mod launch_tests {
    use super::alignment_track_configs_for_launch;
    use rgv_core::{SessionAlignmentTrack, TrackId, TrackSource};

    #[test]
    fn command_line_alignment_track_inherits_the_requested_complete_read_limit() {
        let source = TrackSource::Remote {
            url: "https://example.test/sample.bam".to_owned(),
        };
        let tracks = alignment_track_configs_for_launch(
            Vec::new(),
            Some(source.clone()),
            TrackId::new(9),
            Some("https://example.test/sample.bam.bai".to_owned()),
            10_000_000,
        );

        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].id, TrackId::new(9));
        assert_eq!(tracks[0].source, source);
        assert_eq!(tracks[0].maximum_alignments, 10_000_000);
        assert_eq!(
            tracks[0].remote_index_url.as_deref(),
            Some("https://example.test/sample.bam.bai")
        );
    }

    #[test]
    fn restored_session_alignment_track_keeps_its_own_limit_and_index() {
        let mut session_track = SessionAlignmentTrack::new(
            TrackId::new(4),
            "Session reads".to_owned(),
            TrackSource::Local {
                path: "/session/reads.bam".to_owned(),
            },
        );
        session_track.maximum_alignments = 73_421;
        session_track.remote_index_url = Some("https://session.test/index.bai".to_owned());

        let tracks = alignment_track_configs_for_launch(
            vec![session_track.clone()],
            Some(TrackSource::Local {
                path: "/cli/reads.bam".to_owned(),
            }),
            TrackId::new(99),
            Some("https://cli.test/index.bai".to_owned()),
            10_000_000,
        );

        assert_eq!(tracks, vec![session_track]);
    }
}

#[cfg(test)]
mod capture_tests {
    use super::{decoded_capture_pixels, frame_target_plan};

    #[test]
    fn native_capture_adds_a_target_without_replacing_the_presented_surface() {
        let ordinary = frame_target_plan(false);
        assert!(ordinary.render_surface);
        assert!(!ordinary.render_capture);

        let captured = frame_target_plan(true);
        assert!(captured.render_surface);
        assert!(captured.render_capture);
    }

    #[test]
    fn readback_removes_row_padding_and_converts_bgra_to_rgba() {
        let mut mapped = vec![0_u8; 512];
        mapped[..8].copy_from_slice(&[30, 20, 10, 255, 60, 50, 40, 128]);
        mapped[256..264].copy_from_slice(&[90, 80, 70, 255, 120, 110, 100, 64]);
        let pixels =
            decoded_capture_pixels(&mapped, 2, 2, 256, wgpu::TextureFormat::Bgra8UnormSrgb)
                .unwrap();
        assert_eq!(
            pixels,
            [
                10, 20, 30, 255, 40, 50, 60, 128, 70, 80, 90, 255, 100, 110, 120, 64,
            ]
        );
    }

    #[test]
    fn readback_rejects_non_four_byte_surface_formats() {
        assert!(
            decoded_capture_pixels(&[0; 256], 1, 1, 256, wgpu::TextureFormat::Rgba16Float).is_err()
        );
    }
}
