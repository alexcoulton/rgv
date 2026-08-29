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
        let navigation_preview_renderer =
            NavigationPreviewRenderer::new(&device, surface_config.format);
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
        // The desktop coordinates these as borrowing ceilings, not fixed
        // partitions. Empty derived tiers must not strand most of the user's
        // selected budget before the first alignment query even starts.
        let cache_budgets = CacheBudgets {
            remote_blocks: 0,
            decoded_alignments: total_cache_bytes,
            coverage_summaries: 0,
            layouts: total_cache_bytes,
            gpu_buffers: total_cache_bytes,
        };
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
        let mut alignment_track_configs = options
            .loaded_session
            .as_ref()
            .map_or_else(Vec::new, |session| session.alignment_tracks.clone());
        if alignment_track_configs.is_empty()
            && let Some(source) = alignment_source.clone()
        {
            let next_id = tracks
                .tracks()
                .iter()
                .map(|track| track.id.get())
                .max()
                .unwrap_or_default()
                .saturating_add(1);
            alignment_track_configs.push(SessionAlignmentTrack::new(
                TrackId::new(next_id),
                source_display_name(Some(&source), "Alignments"),
                source,
            ));
            alignment_track_configs[0]
                .remote_index_url
                .clone_from(&options.remote_index_url);
        }
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
        let alignment_cache_bytes = alignment_cache_share(
            cache_budgets.decoded_alignments,
            alignment_track_configs.len(),
        );
        let mut alignment_tracks = Vec::with_capacity(alignment_track_configs.len());
        for config in alignment_track_configs {
            alignment_tracks.push(spawn_alignment_track_controller(
                config,
                alignment_cache_bytes,
                options.reference.clone(),
                remote_reference_index_url.clone(),
                &panel_ids,
            )?);
        }
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
        let alignment_row_layout_scheduler_clock = GenerationClock::default();
        let alignment_row_layout_worker_count = std::thread::available_parallelism()
            .map_or(2, NonZeroUsize::get)
            .clamp(1, 4);
        let alignment_row_layout_scheduler = Scheduler::new(
            NonZeroUsize::new(alignment_row_layout_worker_count)
                .expect("read-layout worker count is nonzero"),
            NonZeroUsize::new(32).expect("read-layout queue capacity is nonzero"),
            &alignment_row_layout_scheduler_clock,
            process_alignment_row_layout_work,
        );
        let alignment_row_scene_scheduler_clock = GenerationClock::default();
        let alignment_row_scene_scheduler = Scheduler::new(
            NonZeroUsize::new(alignment_row_layout_worker_count.min(2))
                .expect("row-scene worker count is nonzero"),
            NonZeroUsize::new(16).expect("row-scene queue capacity is nonzero"),
            &alignment_row_scene_scheduler_clock,
            process_alignment_row_scene_work,
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
            navigation_preview_renderer,
            navigation_texture_revisions: HashMap::new(),
            navigation_horizon_preview_stats: NavigationHorizonPreviewStats::default(),
            navigation_stale_result_rejections: 0,
            alignment_row_layout_cancellations: 0,
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
            alignment_row_layout_tasks: Vec::new(),
            alignment_row_layout_scheduler_clock,
            alignment_row_layout_scheduler,
            alignment_row_scene_tasks: Vec::new(),
            alignment_row_scene_scheduler_clock,
            alignment_row_scene_scheduler,
            reference_engine,
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
            layout_cache: WeightedLruCache::new(NonZeroUsize::new(cache_budgets.layouts).unwrap()),
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
            cache_budgets,
            cache_coordinator: LiveCacheCoordinatorSnapshot {
                total_budget_bytes: total_cache_bytes,
                decoded_allowance_bytes: total_cache_bytes,
                layout_allowance_bytes: total_cache_bytes,
                row_scene_allowance_bytes: total_cache_bytes,
                gpu_slab_allowance_bytes: total_cache_bytes,
                ..LiveCacheCoordinatorSnapshot::default()
            },
            row_scene_residency_cache: None,
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
        preview_instances_empty: bool,
        live_regions: &[[u32; 4]],
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
            self.navigation_preview_renderer.render(&mut pass);
            if preview_instances_empty && live_regions.len() == self.workspace_layout.panels.len() {
                let scissor = self.workspace_layout.physical_genomic_scissor(
                    scale_factor,
                    self.surface_config.width,
                    self.surface_config.height,
                );
                if scissor.width > 0 && scissor.height > 0 {
                    pass.set_scissor_rect(scissor.x, scissor.y, scissor.width, scissor.height);
                    self.rectangle_renderer.render(&mut pass);
                }
            } else {
                for [x, y, width, height] in live_regions.iter().copied() {
                    if width > 0 && height > 0 {
                        pass.set_scissor_rect(x, y, width, height);
                        self.rectangle_renderer.render(&mut pass);
                    }
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
            task.automation_data["navigation_preview_active"] = serde_json::Value::Bool(
                self.panels
                    .iter()
                    .any(|panel| panel.navigation_presentation.shows_retained_preview()),
            );
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
                for panel in self.panels.iter_mut() {
                    panel
                        .navigation_presentation
                        .invalidate_preview("rendering surface was lost");
                    panel.navigation_provisional_display = None;
                }
                self.discard_navigation_preview_texture();
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
        let previous_preview_size = {
            let stats = self.navigation_preview_renderer.stats();
            [stats.width, stats.height]
        };
        let scale_factor = self.window.scale_factor() as f32;
        let has_visible_genomic_canvas = self.workspace_layout.panels.iter().any(|layout| {
            self.workspace_layout
                .physical_canvas_scissor(
                    layout.panel_id,
                    scale_factor,
                    self.surface_config.width,
                    self.surface_config.height,
                )
                .is_some_and(|scissor| scissor.width > 0 && scissor.height > 0)
        });
        let preview_texture_available = if has_visible_genomic_canvas {
            self.navigation_preview_renderer.ensure_texture(
                &self.device,
                self.surface_config.width,
                self.surface_config.height,
            )
        } else {
            self.navigation_preview_renderer.invalidate_texture();
            false
        };
        if !preview_texture_available {
            let reason = if has_visible_genomic_canvas {
                "preview texture exceeds the defensive memory budget"
            } else {
                "no visible non-empty genomic canvas"
            };
            for panel in self.panels.iter_mut() {
                if panel.navigation_presentation.source.is_some() {
                    panel.navigation_presentation.invalidate_preview(reason);
                    panel.navigation_provisional_display = None;
                }
            }
        }
        if previous_preview_size != [self.surface_config.width, self.surface_config.height] {
            self.navigation_texture_revisions.clear();
        }
        let mut capture_regions = Vec::new();
        let mut capture_revisions = Vec::new();
        let mut live_regions = Vec::new();
        let mut preview_instances = Vec::new();
        let mut preview_frame_records = Vec::new();
        for layout in &self.workspace_layout.panels {
            let Some(panel) = self.panels.get(layout.panel_id) else {
                continue;
            };
            let Some(scissor) = self.workspace_layout.physical_canvas_scissor(
                panel.id,
                scale_factor,
                self.surface_config.width,
                self.surface_config.height,
            ) else {
                continue;
            };
            let region = [scissor.x, scissor.y, scissor.width, scissor.height];
            if panel.navigation_presentation.permits_staged_display() {
                live_regions.push(region);
                if prepared_stats.is_some()
                    && preview_texture_available
                    && self.panel_navigation_data_is_settled(panel.id)
                {
                    let content_revision = panel
                        .navigation_presentation
                        .committed_content_revision
                        .saturating_add(1)
                        .max(1);
                    capture_regions.push(region);
                    capture_revisions.push((panel.id, content_revision));
                }
                continue;
            }

            let source = panel
                .navigation_presentation
                .source
                .as_ref()
                .filter(|source| {
                    panel.navigation_presentation.shows_retained_preview()
                        && source.layout_revision == self.workspace_layout.revision
                        && source.contract_revision == self.navigation_contract_revision
                        && self.navigation_texture_revisions.get(&panel.id)
                            == Some(&source.content_revision)
                        && preview_texture_available
                })
                .map(|source| (source.exact_start, source.exact_start + source.span));
            let (source_origin, source_size) = navigation_preview_source_uv(
                [scissor.x, scissor.y],
                [scissor.width, scissor.height],
                [self.surface_config.width, self.surface_config.height],
            );
            preview_instances.push(NavigationPreviewInstance::new(
                [scissor.x as f32, scissor.y as f32],
                [scissor.width as f32, scissor.height as f32],
                source_origin,
                source_size,
                source,
                (
                    panel.navigation_presentation.target_exact_start,
                    panel.navigation_presentation.target_exact_start
                        + panel.navigation_presentation.target_span,
                ),
            ));
            preview_frame_records.push((
                panel.id,
                source.is_none() && panel.navigation_presentation.committed_interval.is_some(),
            ));
            if matches!(
                panel.navigation_presentation.preview_kind,
                Some(NavigationPreviewKind::CompleteHorizon)
            ) && panel.navigation_provisional_display.is_some()
            {
                live_regions.push(region);
            }
        }
        for (panel_id, neutral_after_commit) in preview_frame_records {
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel
                    .navigation_query_schedule
                    .record_preview_frame(neutral_after_commit);
            }
        }
        if preview_texture_available && !capture_regions.is_empty() {
            self.navigation_preview_renderer.capture_committed_regions(
                &mut encoder,
                &self.rectangle_renderer,
                &capture_regions,
            );
        }
        let targets = frame_target_plan(capture_texture.is_some());
        let composite_targets =
            u64::from(targets.render_surface).saturating_add(u64::from(targets.render_capture));
        self.navigation_preview_renderer.prepare(
            &self.device,
            &self.queue,
            [self.surface_config.width, self.surface_config.height],
            &preview_instances,
            composite_targets,
        );
        if targets.render_surface {
            self.render_composed_frame_target(
                &mut encoder,
                &surface_view,
                &paint_jobs,
                &screen_descriptor,
                preview_instances.is_empty(),
                &live_regions,
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
                preview_instances.is_empty(),
                &live_regions,
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
        for (panel_id, content_revision) in &capture_revisions {
            self.navigation_texture_revisions
                .insert(*panel_id, *content_revision);
        }
        let navigation_committed = self.finish_navigation_commits();
        for (panel_id, content_revision) in capture_revisions {
            if let Some(panel) = self.panels.get_mut(panel_id) {
                let capture_settled = panel.navigation_presentation.record_settled_capture(
                    panel.viewport,
                    panel.generation_clock.current().get(),
                    content_revision,
                    self.workspace_layout.revision,
                    self.navigation_contract_revision,
                );
                if capture_settled {
                    crate::display::synchronize_junction_interaction_revision(
                        &mut panel.junction_arc_targets,
                        &mut panel.hovered_junction_arc,
                        content_revision,
                    );
                }
            }
        }
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
