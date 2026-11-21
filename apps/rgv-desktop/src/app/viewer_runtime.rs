use crate::app::tasks::NativeWindowCaptureTask;
use crate::automation::{AutomationAuxiliaryOpenState, AutomationRunner};
use crate::control_service::{ControlService, ControlStatusMetadata};
use crate::diagnostics::LatencyEvent;
use crate::diagnostics::{Diagnostics, HealthHandle, InteractionTraceStage};
use crate::diagnostics::{ManualDiagnosticFramePhase, object_fields};
use crate::display::alignment::persisted_style_tag;
use crate::launch::{LaunchOptions, default_settings_path};
use crate::sources::{
    build_track_specs, load_assembly, opened_project_status, primary_alignment_track,
    reference_open_options, source_display_name, source_path, spawn_track_engine_without_aliases,
    track_source,
};
use crate::ui::{
    CytobandAvailability, GenomeContextMetrics, MENU_HEIGHT, NAVIGATION_HEIGHT, STATUS_HEIGHT,
    WorkspaceLayoutInput, WorkspaceLayoutSnapshot, WorkspacePanelLayoutInput,
};
use crate::workspace::alignments::{
    AlignmentSceneBudgetSnapshot, EmptyAlignmentState, initial_filters,
    spawn_alignment_track_controller,
};
use crate::workspace::navigation::{launch_contig_aliases, standalone_locus};
use crate::workspace::navigation_transaction::NavigationPresentationPhase;
use crate::workspace::panels::{INITIAL_PANEL_ID, PanelWorkspace, ViewPanelController};
use crate::{
    AlignmentGpuPrefetchState, AlignmentGpuSpeculationGate, AlignmentScenePublicationAuthority,
    INITIAL_INTERVAL_END, INITIAL_INTERVAL_START, RemoteSourceEditor, SYNTHETIC_ALIGNMENT_TRACK_ID,
    SYNTHETIC_CONTIG_LENGTH, SashimiInteractionDiagnostics, UiPanel, VariantPredicateOperator,
    Viewer, alignment_gpu_bundle_is_current, app, apply_accessibility_preferences, ui,
};
use anyhow::{Context as _, Result};
use egui_wgpu::{RendererOptions, ScreenDescriptor};
use rgv_alignment_scene::{AlignmentPanelSceneStore, AllocationLedger};
use rgv_automation::{AutomationEnvironmentTelemetry, AutomationErrorCode};
use rgv_core::{
    ContigId, GeneticCode, GenomicInterval, ReferenceDictionary, SessionAlignmentTrack,
    SessionDocument, SessionFilters, SessionPaneTrackState, TrackId, TrackSource,
    VariantFieldScope, VariantMissingValueRule, Viewport, WorkspacePresentation,
};
use rgv_engine::{ReferenceEngine, TrackSearchResults};
use rgv_layout::DisplayList;
use rgv_render::{
    AlignmentGpuPrepareStats, AlignmentGpuRenderer, AlignmentSummaryGpuPrepareStats,
    AlignmentSummaryGpuRenderer, ContextSceneGpuFrame, ContextSceneGpuRenderer, PrepareStats,
    RectangleRenderer,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::keyboard::ModifiersState;
use winit::window::Window;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameTargetPlan {
    render_surface: bool,
    render_capture: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AlignmentRendererFailure {
    lane: &'static str,
    message: String,
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

/// Bookkeeping for device recovery and error reporting (`Viewer::gpu_recovery`), so a
/// persistently failing GPU neither spins the event loop nor floods the notification log.
/// The health record itself is `Viewer::gpu_device_health`: an `Arc` shared with the wgpu
/// callbacks, which outlive any borrow of the viewer. Each (re)created device is registered
/// with [`rgv_render::GpuDeviceHealth::watch`].
#[derive(Debug, Default)]
pub(crate) struct GpuRecoveryState {
    last_attempt: Option<Instant>,
    loss_reported: bool,
    uncaptured_errors_reported: u32,
}

/// Minimum spacing between attempts to recreate a lost GPU device.
const GPU_RECOVERY_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// Uncaptured GPU errors shown to the user per device; all of them are always logged.
const MAX_REPORTED_UNCAPTURED_GPU_ERRORS: u32 = 3;

/// When the event loop must next redraw so that a lost device is retried without waiting for
/// user input: immediately if no attempt has been made, otherwise one retry interval after
/// the last one. `None` while the device is healthy.
fn gpu_recovery_wake(lost: bool, last_attempt: Option<Instant>, now: Instant) -> Option<Instant> {
    lost.then(|| last_attempt.map_or(now, |last| last + GPU_RECOVERY_RETRY_INTERVAL))
}

/// Whether enough time has passed since the previous recovery attempt to try again.
fn gpu_recovery_attempt_is_due(last_attempt: Option<Instant>, now: Instant) -> bool {
    last_attempt
        .is_none_or(|last| now.saturating_duration_since(last) >= GPU_RECOVERY_RETRY_INTERVAL)
}

/// One step of the adapter fallback chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdapterRequest {
    power_preference: wgpu::PowerPreference,
    force_fallback_adapter: bool,
}

/// The adapters to try, in order: the requested power preference (`WGPU_POWER_PREF`, otherwise
/// high performance), then the other hardware class, then whatever the backend offers, and
/// finally a software rasteriser. A discrete GPU is preferred but never required, so RGV still
/// starts on integrated graphics, remote desktops and login nodes that only expose llvmpipe,
/// lavapipe or WARP.
fn adapter_request_plan(requested: Option<wgpu::PowerPreference>) -> Vec<AdapterRequest> {
    let preferred = requested.unwrap_or(wgpu::PowerPreference::HighPerformance);
    let alternative = match preferred {
        wgpu::PowerPreference::HighPerformance => wgpu::PowerPreference::LowPower,
        wgpu::PowerPreference::LowPower | wgpu::PowerPreference::None => {
            wgpu::PowerPreference::HighPerformance
        }
    };
    let mut plan = Vec::with_capacity(4);
    for power_preference in [preferred, alternative, wgpu::PowerPreference::None] {
        let request = AdapterRequest {
            power_preference,
            force_fallback_adapter: false,
        };
        if !plan.contains(&request) {
            plan.push(request);
        }
    }
    plan.push(AdapterRequest {
        power_preference: wgpu::PowerPreference::None,
        force_fallback_adapter: true,
    });
    plan
}

/// Limits requested from an adapter: WebGPU's downlevel defaults, which every renderer here fits
/// inside (one dynamic uniform binding of well under 16 KiB, at most eight vertex attributes, no
/// storage buffers or compute), with the adapter's own texture resolution so large windows and
/// the egui font atlas are not artificially capped at 2048 pixels. `Limits::default()` asks for
/// more than software and older integrated adapters provide and made device creation fail there.
fn required_gpu_limits(adapter_limits: wgpu::Limits) -> wgpu::Limits {
    wgpu::Limits::downlevel_defaults().using_resolution(adapter_limits)
}

/// Surface-format policy.
///
/// RGV renders to a non-sRGB 8-bit target (`Bgra8Unorm` or `Rgba8Unorm`) whenever the surface
/// offers one:
///
/// * every colour in the renderers' shaders and palettes is authored as a display-referred
///   (sRGB-encoded) value and written to the target unchanged, exactly as the CPU PNG/SVG export
///   writes the same numbers; an sRGB target would treat them as linear light and encode them
///   again, so the window would be visibly lighter than the export of the same view;
/// * alpha blending then happens on encoded values, which is what egui expects, and egui-wgpu
///   itself prefers these formats (`egui_wgpu::preferred_framebuffer_format`), using its
///   gamma-framebuffer fragment path for them.
///
/// `Surface::get_default_config` simply takes the backend's first format, which is sRGB on some
/// backends and not on others, so the same build looked different per platform. If no non-sRGB
/// 8-bit format exists the sRGB 8-bit ones are accepted (lighter colours, still correct
/// captures), and only then whatever the surface lists first.
fn choose_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    // Within each class the surface's own ordering, which is its preference, is kept.
    let first_of = |class: [wgpu::TextureFormat; 2]| {
        formats
            .iter()
            .copied()
            .find(|format| class.contains(format))
    };
    first_of([
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8Unorm,
    ])
    .or_else(|| {
        first_of([
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ])
    })
    .or_else(|| formats.first().copied())
}

/// A surface with the device chosen for it. The surface is not configured yet.
struct GpuContext {
    surface: wgpu::Surface<'static>,
    adapter_info: wgpu::AdapterInfo,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
}

/// Requests every adapter in the fallback chain that can present to `surface`, most preferred
/// first, recording why each unavailable one was skipped. `WGPU_ADAPTER_NAME`, when set, names
/// the first candidate.
async fn gpu_adapter_candidates(
    instance: &wgpu::Instance,
    surface: &wgpu::Surface<'static>,
    failures: &mut Vec<String>,
) -> Vec<wgpu::Adapter> {
    let mut candidates = Vec::new();
    if std::env::var_os("WGPU_ADAPTER_NAME").is_some() {
        match wgpu::util::initialize_adapter_from_env(instance, Some(surface)).await {
            Ok(adapter) => candidates.push(adapter),
            Err(error) => failures.push(format!("WGPU_ADAPTER_NAME: {error}")),
        }
    }
    for request in adapter_request_plan(wgpu::PowerPreference::from_env()) {
        match instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: request.power_preference,
                force_fallback_adapter: request.force_fallback_adapter,
                compatible_surface: Some(surface),
            })
            .await
        {
            Ok(adapter) => candidates.push(adapter),
            Err(error) => failures.push(format!(
                "{:?}{}: {error}",
                request.power_preference,
                if request.force_fallback_adapter {
                    " (software fallback)"
                } else {
                    ""
                }
            )),
        }
    }
    candidates
}

/// Creates the surface for `window` and the first adapter and device in the fallback chain that
/// can present to it. Honours `WGPU_BACKEND` (and wgpu's other instance variables),
/// `WGPU_ADAPTER_NAME` and `WGPU_POWER_PREF`. The returned device is already watched by
/// `health`.
async fn create_gpu_context(
    window: &Arc<Window>,
    size: PhysicalSize<u32>,
    health: &Arc<rgv_render::GpuDeviceHealth>,
) -> Result<GpuContext> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let surface = instance
        .create_surface(Arc::clone(window))
        .context("failed to create GPU surface")?;

    let mut failures = Vec::new();
    let candidates = gpu_adapter_candidates(&instance, &surface, &mut failures).await;

    let mut tried = Vec::<wgpu::AdapterInfo>::new();
    for adapter in candidates {
        let info = adapter.get_info();
        // Different requests frequently resolve to the same adapter.
        if tried.iter().any(|previous| {
            previous.backend == info.backend
                && previous.vendor == info.vendor
                && previous.device == info.device
                && previous.name == info.name
        }) {
            continue;
        }
        let description = format!("{} ({:?})", info.name, info.backend);
        let capabilities = surface.get_capabilities(&adapter);
        let Some(format) = choose_surface_format(&capabilities.formats) else {
            failures.push(format!("{description}: cannot present to this window"));
            tried.push(info);
            continue;
        };
        let Some(mut surface_config) =
            surface.get_default_config(&adapter, size.width.max(1), size.height.max(1))
        else {
            failures.push(format!("{description}: no supported surface configuration"));
            tried.push(info);
            continue;
        };
        surface_config.format = format;
        surface_config.present_mode = wgpu::PresentMode::AutoVsync;
        surface_config.desired_maximum_frame_latency = 2;
        match adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("RGV GPU device"),
                required_features: wgpu::Features::empty(),
                required_limits: required_gpu_limits(adapter.limits()),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
            })
            .await
        {
            Ok((device, queue)) => {
                tracing::info!(
                    name = info.name,
                    backend = ?info.backend,
                    device_type = ?info.device_type,
                    surface_format = ?format,
                    "selected GPU adapter"
                );
                health.watch(&device);
                return Ok(GpuContext {
                    surface,
                    adapter_info: info,
                    device,
                    queue,
                    surface_config,
                });
            }
            Err(error) => {
                failures.push(format!("{description}: {error}"));
                tried.push(info);
            }
        }
    }
    anyhow::bail!(
        "no usable GPU adapter was found for this display. RGV needs Vulkan, Metal, Direct3D 12 \
         or OpenGL; a software rasteriser such as Mesa lavapipe/llvmpipe is sufficient. Set \
         WGPU_BACKEND (vulkan, metal, dx12 or gl) to force a backend. Tried: {}",
        if failures.is_empty() {
            "no adapters were offered".to_owned()
        } else {
            failures.join("; ")
        }
    )
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
        let gpu_device_health = Arc::new(rgv_render::GpuDeviceHealth::new());
        let GpuContext {
            surface,
            adapter_info,
            device,
            queue,
            surface_config,
        } = create_gpu_context(&window, size, &gpu_device_health).await?;
        surface.configure(&device, &surface_config);

        let rectangle_renderer = RectangleRenderer::new(&device, surface_config.format);
        let context_scene_gpu_renderer =
            ContextSceneGpuRenderer::new(&device, surface_config.format);
        // The palette is part of renderer construction, so the first frame is already themed.
        let initial_theme =
            crate::theme::DesktopTheme::new(crate::theme::theme_id(options.loaded_settings.theme));
        let alignment_gpu_renderer = AlignmentGpuRenderer::with_palette(
            &device,
            surface_config.format,
            initial_theme.palette(),
        );
        let alignment_summary_gpu_renderer = AlignmentSummaryGpuRenderer::with_palette(
            &device,
            surface_config.format,
            initial_theme.palette(),
        );
        let mut alignment_depth_pyramid_gpu_renderer =
            rgv_render::AlignmentDepthPyramidGpuRenderer::with_palette(
                &device,
                surface_config.format,
                initial_theme.palette(),
            );
        let egui_context = egui::Context::default();
        egui_context.set_visuals(initial_theme.visuals());
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
        let options_settings_recovery = options.loaded_settings_recovery.clone();
        apply_accessibility_preferences(&egui_context, &settings);
        if let Some(path) = options.session.as_ref().or(options.import_igv.as_ref()) {
            settings.remember_session(path.display().to_string());
        }
        let total_cache_bytes = settings.cache_budget_mib.saturating_mul(1024 * 1024).max(1);
        // One enforcing authority is shared by every alignment source, immutable scene layer, and
        // derived GPU page. The explicit transition reserve permits one old and one candidate
        // visible set; transient decoder/upload work is reported under its own equal ceiling.
        // The memory policy is read once: the renderers' leases belong to the ledger they were
        // charged to, so a change of policy takes effect at the next launch.
        let alignment_scene_ledger = AllocationLedger::with_policy(
            settings.alignment_memory_policy,
            total_cache_bytes,
            total_cache_bytes,
            total_cache_bytes,
        );
        let alignment_ledger_progress = alignment_scene_ledger.subscribe_progress();
        let alignment_gpu_ledger = crate::workspace::alignment_management::alignment_gpu_ledger_for(
            &alignment_scene_ledger,
            total_cache_bytes,
        );
        let alignment_gpu_ledger_progress = alignment_gpu_ledger.subscribe_progress();
        // Pyramid pages are charged to the GPU ledger with every other alignment page.
        alignment_depth_pyramid_gpu_renderer.set_ledger(alignment_gpu_ledger.clone());
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
            |session| -> Result<PanelWorkspace> {
                let panel_count = session.panels.len().max(1);
                let panel_canvas_width = bootstrap_layout.projected_canvas_width(panel_count);
                let mut panels = PanelWorkspace::from_session(
                    session,
                    panel_canvas_width,
                    canvas_geometry.viewport.top(),
                )?;
                if let Some(request) = &options.loaded_igv_locus {
                    panels.adopt_imported_locus(request)?;
                }
                Ok(panels)
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
        let track_engine = (!track_specifications.is_empty())
            .then(|| spawn_track_engine_without_aliases(track_specifications));
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
        // One persistent coverage cache for the process, unless the user turned it off.
        let coverage_sidecar = settings
            .coverage_cache_enabled
            .then(rgv_hts::CoverageSidecarCache::open_default)
            .flatten()
            .map(Arc::new);
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
        crate::workspace::alignments::connect_reference_slice_sinks(
            reference_engine.as_ref(),
            &alignment_tracks,
        );
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
        let stress_health = HealthHandle::new(application_instance_id.clone());
        // Everything a control client makes RGV open is held to these rules (APP-8).
        let control_source_policy = options.control_port.and_then(|_| {
            options
                .control_root
                .clone()
                .map(|root| app::tasks::ControlSourcePolicy {
                    root,
                    allow_private_network_urls: options.control_allow_private_urls,
                })
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
                    stress_health: stress_health.clone(),
                    allow_private_network_urls: options.control_allow_private_urls,
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
        let settings_path = options
            .settings_path
            .clone()
            .unwrap_or_else(default_settings_path);
        let diagnostic_trace_directory =
            options.diagnostic_trace_dir.clone().unwrap_or_else(|| {
                settings_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("diagnostic-traces")
            });
        let start_diagnostic_trace = options.start_diagnostic_trace;
        let mut viewer = Self {
            session: rgv_session::Session {
                workspace_revision: 1,
                document_state: app::document_state::DocumentState::default(),
                navigation_stale_result_rejections: 0,
                panels,
                engine_retirement_tasks: Vec::new(),
                reference_engine,
                reference_revision: None,
                reference_open_error: None,
                track_engine,
                tracks,
                track_presentation,
                reference_source,
                remote_reference_index_url,
                reference_dictionary: None,
                contig_aliases,
                assembly,
                assembly_identity,
                suggested_assembly_identity: None,
                suppressed_builtin_gene_bundles: options
                    .loaded_session
                    .as_ref()
                    .map_or_else(BTreeSet::new, |session| {
                        session.suppressed_builtin_gene_bundles.clone()
                    }),
                assembly_name: options.genome_name,
                cytoband_path: options.cytobands,
                data_status,
                busy_status: app::status_activity::BusyStatus::default(),
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
                synthetic,
                navigation_contract_revision: 0,
                settings,
                settings_writer: app::settings_writer::SettingsWriter::new(settings_path.clone()),
                settings_path,
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
                imported_locus: options.loaded_igv_locus,
                imported_feature_search: None,
                undo: rgv_session::action::history::UndoHistory::default(),
                notifications: app::shell_state::NotificationLog::default(),
                navigation_gesture: app::shell_state::NavigationGestureCommit::default(),
                track_ids: app::shell_state::TrackIdAllocator::default(),
                last_autosaved: None,
                skipped_lines: rgv_session::track_diagnostics::SkippedLineReporter::default(),
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
                alignment_coverage_retirement_tasks: Vec::new(),
                alignment_scene_ledger,
                alignment_scene_budget: initial_alignment_scene_budget,
                workspace_alignment_limit,
                alignment_scene_identities: Default::default(),
            },
            application_instance_id,
            shell: app::shell_state::ShellState::default(),
            read_inspector_derived: std::cell::RefCell::default(),
            gpu_device_health,
            gpu_recovery: GpuRecoveryState::default(),
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
            alignment_ledger_progress,
            alignment_gpu_ledger,
            alignment_gpu_ledger_progress,
            alignment_gpu_renderer,
            alignment_summary_gpu_renderer,
            alignment_gpu_last_prepare_stats: AlignmentGpuPrepareStats::default(),
            alignment_summary_gpu_last_prepare_stats: AlignmentSummaryGpuPrepareStats::default(),
            alignment_presented_epoch: None,
            alignment_gpu_residency_bundle: None,
            alignment_gpu_prefetch: AlignmentGpuPrefetchState::default(),
            alignment_gpu_publication_retry: None,
            alignment_last_gpu_budget_pressure: None,
            alignment_gpu_partial_top_up: None,
            alignment_gpu_top_up_in_progress: false,
            alignment_capacity_reclaim: None,
            alignment_capacity_reclaim_clock: 0,
            alignment_gpu_speculation_gate: AlignmentGpuSpeculationGate::default(),
            alignment_panel_scene_stores,
            alignment_scene_publication_authority: AlignmentScenePublicationAuthority::default(),
            egui_context,
            egui_state,
            egui_renderer,
            workspace_layout,
            genomic_canvas_layer_id: None,
            pane_divider_rect: None,
            composite_display_list: DisplayList::default(),
            shared_annotation_frame_heights: BTreeMap::new(),
            alignment_coverage_sidecar: coverage_sidecar,
            alignment_depth_pyramid_gpu_renderer,
            alignment_depth_pyramid_gpu_frame: None,
            alignment_depth_pyramid_pages_refused: BTreeMap::new(),
            automation_auxiliary_open_state,
            reference_remote_io_stats: None,
            landmark_resource_root: options.landmark_resource_root.clone(),
            landmark_catalogue: options.loaded_landmark_catalogue.clone(),
            landmark_catalogue_error: options.landmark_catalogue_error.clone(),
            landmark_resolution: None,
            landmark_search_index: None,
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
            genome_tasks: Vec::new(),
            landmark_install_ticket: 0,
            hosted_genomes: crate::workspace::genomes::HostedGenomes::default(),
            promoted_read: None,
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
            diagnostics: Diagnostics::new(
                stress_health,
                diagnostic_trace_directory,
                adapter_info.name.clone(),
                format!("{:?}", adapter_info.backend),
            ),
            manual_diagnostic_trace_autostart_pending: start_diagnostic_trace,
            genome_overview_cache: BTreeMap::new(),
            genome_overview_recomputations: 0,
            sashimi_interaction_diagnostics: SashimiInteractionDiagnostics::default(),
            command_search: String::new(),
            annotation_search_generation: 0,
            annotation_navigation_generation: 0,
            annotation_search_results: TrackSearchResults::default(),
            pending_annotation_navigation: None,
            next_autosave,
            recovery_candidates: options.recovery_candidates,
            recovery_open,
            automation,
            control_service,
            control_source_policy,
            automation_exit_requested: false,
            exit_confirmation_open: false,
            user_exit_requested: false,
            context_lane_gpu_budget_bytes,
            track_tiles: crate::workspace::track_tiles::TrackTilePipelineState::default(),
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
            alignment_context_menu: None,
        };
        if start_diagnostic_trace
            && (viewer.session.synthetic || viewer.session.alignment_tracks.is_empty())
        {
            viewer.manual_diagnostic_trace_autostart_pending = false;
            viewer.start_manual_diagnostic_trace("launch option", true);
        }
        viewer
            .session
            .document_state
            .replace(viewer.session_document(), true);
        let launch_session_path = viewer.session.current_session_path.clone();
        viewer.arm_newer_writer_guard(launch_session_path.as_deref());
        if let Some(recovery) = &options_settings_recovery {
            let notice = app::settings_writer::settings_recovery_notice(
                &viewer.session.settings_path,
                recovery,
            );
            viewer.report_warning(notice);
        }
        Ok(viewer)
    }

    pub(crate) fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.surface_config.width = size.width;
        self.surface_config.height = size.height;
        if let Some(service) = &self.control_service {
            service.update_window_metrics(size.width, size.height, self.window.scale_factor());
        }
        self.reconfigure_surface();
        // The next egui frame owns the final shell rectangle, including its
        // persisted Properties width. Keep the currently displayed geometry
        // committed until that frame can replace the UI and genomic display
        // list together.
        self.mark_display_dirty("explicit display invalidation");
        self.window.request_redraw();
    }

    pub(crate) fn reconfigure_surface(&self) {
        if self.surface_config.width > 0 && self.surface_config.height > 0 {
            self.surface.configure(&self.device, &self.surface_config);
        }
    }

    /// Checks the GPU device before a frame is submitted and returns whether the frame may be
    /// rendered. Uncaptured wgpu errors are logged and surfaced instead of aborting the process.
    /// If the device was lost, a replacement device, surface and set of renderers is created (at
    /// most once per [`GPU_RECOVERY_RETRY_INTERVAL`]); the frame in hand is dropped either way,
    /// because it was laid out against the lost device, and a fresh one is requested after a
    /// successful recovery. While recovery keeps failing the window simply stops updating and
    /// the failure is shown once in the status line and notification log.
    pub(crate) fn ensure_gpu_device(&mut self) -> bool {
        let health = Arc::clone(&self.gpu_device_health);
        if let Some(error) = health.take_last_error() {
            tracing::error!(%error, "uncaptured GPU error");
            let report = {
                let state = &mut self.gpu_recovery;
                let report = state.uncaptured_errors_reported < MAX_REPORTED_UNCAPTURED_GPU_ERRORS;
                state.uncaptured_errors_reported =
                    state.uncaptured_errors_reported.saturating_add(1);
                report
            };
            if report && !health.is_lost() {
                self.report_error(format!("GPU error: {error}"));
            }
        }
        if !health.is_lost() {
            return true;
        }
        let now = Instant::now();
        let (due, first_report) = {
            let state = &mut self.gpu_recovery;
            let due = gpu_recovery_attempt_is_due(state.last_attempt, now);
            if due {
                state.last_attempt = Some(now);
            }
            let first_report = !state.loss_reported;
            state.loss_reported = true;
            (due, first_report)
        };
        let cause = health
            .loss()
            .map_or_else(|| "unknown cause".to_owned(), |loss| loss.message);
        if first_report {
            tracing::error!(%cause, "GPU device lost");
        }
        if !due {
            return false;
        }
        match self.recover_gpu_device() {
            Ok(()) => {
                self.gpu_recovery = GpuRecoveryState::default();
                tracing::warn!(%cause, "GPU device was lost and has been recreated");
                self.report_error(format!(
                    "The GPU device was lost ({cause}) and has been recreated."
                ));
                self.mark_display_dirty("explicit display invalidation");
                self.window.request_redraw();
            }
            Err(error) => {
                tracing::error!(%error, "could not recreate the lost GPU device");
                if first_report {
                    self.report_error(format!(
                        "The GPU device was lost ({cause}) and could not be recreated: {error:#}. \
                         Rendering is paused; RGV retries every second."
                    ));
                }
            }
        }
        false
    }

    /// When the event loop should next redraw to retry recreating a lost GPU device, so that
    /// recovery does not depend on the user moving the mouse. `None` while the device is healthy.
    pub(crate) fn gpu_recovery_deadline(&self, now: Instant) -> Option<Instant> {
        gpu_recovery_wake(
            self.gpu_device_health.is_lost(),
            self.gpu_recovery.last_attempt,
            now,
        )
    }

    /// Replaces the lost device with a new one and rebuilds everything derived from it. Semantic
    /// scenes, sessions and selections are untouched; GPU pages are admitted again on demand.
    fn recover_gpu_device(&mut self) -> Result<()> {
        let GpuContext {
            surface,
            adapter_info: _,
            device,
            queue,
            mut surface_config,
        } = pollster::block_on(create_gpu_context(
            &self.window,
            self.window.inner_size(),
            &self.gpu_device_health,
        ))?;
        let format = surface_config.format;
        if self.surface_config.width > 0 && self.surface_config.height > 0 {
            surface_config.width = self.surface_config.width;
            surface_config.height = self.surface_config.height;
        }

        // Nothing captured against the lost device may be replayed.
        //
        // ORDER MATTERS. Ledger charges for GPU pages are reference-counted through frame pins,
        // so every holder of GPU frames, residency leases, prefetch cursors or retirements must
        // be dropped BEFORE `rebuild_for_device`. A holder that survives the rebuild keeps the
        // old device's charges pinned (and releases them later against the new renderer's
        // accounting). Add any new holder to the block of resets above the rebuild calls.
        self.alignment_presented_epoch = None;
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_publication_retry = None;
        self.alignment_capacity_reclaim = None;
        self.alignment_gpu_speculation_gate.reset();
        self.alignment_gpu_renderer
            .rebuild_for_device(&device, format);
        self.alignment_summary_gpu_renderer
            .rebuild_for_device(&device, format);
        // Pyramid pages are plain uploads of resident CPU tiles: a new renderer re-uploads them
        // on its first frame, charged to the GPU ledger like the pages it replaces (whose
        // charges end with the old renderer).
        self.alignment_depth_pyramid_gpu_frame = None;
        self.alignment_depth_pyramid_gpu_renderer =
            rgv_render::AlignmentDepthPyramidGpuRenderer::with_palette(
                &device,
                format,
                crate::theme::active().palette(),
            );
        self.alignment_depth_pyramid_gpu_renderer
            .set_ledger(self.alignment_gpu_ledger.clone());
        self.alignment_depth_pyramid_pages_refused.clear();
        self.context_scene_gpu_renderer = ContextSceneGpuRenderer::new(&device, format);
        self.context_scene_gpu_frame = ContextSceneGpuFrame::default();
        // Track tile pages are plain uploads of the engine's cached scenes: the renderer (and
        // its ledger) is rebuilt for the new device on the next frame that has a routed lane.
        self.track_tiles.device_lost();
        self.rectangle_renderer = RectangleRenderer::new(&device, format);
        self.egui_renderer = egui_wgpu::Renderer::new(&device, format, RendererOptions::default());
        // The new egui renderer has none of the textures egui believes it already delivered.
        // Rebuilding the font atlas makes the next frame resend it in full.
        self.egui_context
            .set_fonts(egui::FontDefinitions::default());

        // Drop the old surface (and its swapchain) before configuring the replacement: a window
        // can back only one live swapchain on some backends.
        self.surface = surface;
        self.device = device;
        self.queue = queue;
        self.surface_config = surface_config;
        self.reconfigure_surface();
        Ok(())
    }

    /// Draws the coverage pyramid below the exact lanes: where both exist the pyramid is
    /// scissored to the spans the exact coverage lane does not draw.
    fn render_alignment_coverage_pyramid<'pass>(
        &'pass self,
        pass: &mut wgpu::RenderPass<'pass>,
        target_size: [u32; 2],
        failures: &mut Vec<AlignmentRendererFailure>,
    ) {
        if let Some(frame) = &self.alignment_depth_pyramid_gpu_frame
            && let Err(error) =
                self.alignment_depth_pyramid_gpu_renderer
                    .render_within(pass, frame, target_size)
        {
            tracing::error!(%error, "could not render the alignment coverage pyramid");
            failures.push(AlignmentRendererFailure {
                lane: "coverage pyramid",
                message: error.to_string(),
            });
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
    ) -> Vec<AlignmentRendererFailure> {
        let mut failures = Vec::new();
        // Both the surface and the capture texture are created at the configured surface size.
        // Captured scissors are intersected with it at replay time (REND-12): a frame captured
        // before a resize must not carry a scissor outside the attachment into the pass.
        let target_size = [self.surface_config.width, self.surface_config.height];
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("RGV genomic canvas"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(crate::theme::active().canvas_clear()),
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
            if let Some([x, y, width, height]) = rgv_render::clamp_scissor_to_target(
                [scissor.x, scissor.y, scissor.width, scissor.height],
                target_size,
            ) {
                pass.set_scissor_rect(x, y, width, height);
                self.rectangle_renderer.render(&mut pass);
                // Lanes of the experimental tile pipeline occupy the position of the legacy
                // context lanes they replace. Each draw is scissored to its tile's columns of
                // its lane; the genomic scissor is restored for the renderers that follow.
                if self.track_tiles.render(&mut pass) {
                    pass.set_scissor_rect(x, y, width, height);
                }
                self.context_scene_gpu_renderer.render_within(
                    &mut pass,
                    &self.context_scene_gpu_frame,
                    target_size,
                );
            }
            self.render_alignment_coverage_pyramid(&mut pass, target_size, &mut failures);
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
                    && let Err(error) = self.alignment_summary_gpu_renderer.render_within(
                        &mut pass,
                        frame,
                        target_size,
                    )
                {
                    tracing::error!(
                        %error,
                        "could not render unified alignment summary frame"
                    );
                    failures.push(AlignmentRendererFailure {
                        lane: "summaries",
                        message: error.to_string(),
                    });
                }
                if let Some(frame) = &bundle.reads
                    && let Err(error) =
                        self.alignment_gpu_renderer
                            .render_within(&mut pass, frame, target_size)
                {
                    tracing::error!(%error, "could not render unified alignment scene frame");
                    failures.push(AlignmentRendererFailure {
                        lane: "reads",
                        message: error.to_string(),
                    });
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
        failures
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
        if !self.ensure_gpu_device() {
            // The device was lost (and possibly just recreated): this frame's tessellation and
            // prepared buffers belong to the old device, so it is dropped, not presented.
            return;
        }
        self.prepare_alignment_coverage_pyramid_frame(self.window.scale_factor() as f32);
        self.prepare_track_tile_frame(self.window.scale_factor() as f32);
        let paint_jobs = self
            .egui_context
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::Tessellation,
            tessellation_started.elapsed(),
        );
        if self.diagnostics.latency_active() {
            self.diagnostics.latency(LatencyEvent::Phase(
                InteractionTraceStage::EguiTessellationFinished,
                tessellation_started.elapsed(),
            ));
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
                serde_json::Value::Bool(self.session.panels.iter().any(|panel| {
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
                self.diagnostics.frame_phase(
                    ManualDiagnosticFramePhase::SurfaceAcquisition,
                    surface_started.elapsed(),
                );
                self.pending_native_window_capture = capture_task;
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                if self.automation.is_none() {
                    self.diagnostics.frame_phase(
                        ManualDiagnosticFramePhase::SurfaceAcquisition,
                        surface_started.elapsed(),
                    );
                    self.pending_native_window_capture = capture_task;
                    return;
                }
                // Native automation may be fully covered by the controlling
                // application. Render the same authoritative frame to a
                // private target so transactional navigation and deterministic
                // GPU captures can progress without stealing focus.
                None
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.diagnostics.frame_phase(
                    ManualDiagnosticFramePhase::SurfaceAcquisition,
                    surface_started.elapsed(),
                );
                self.pending_native_window_capture = capture_task;
                self.reconfigure_surface();
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.diagnostics.frame_phase(
                    ManualDiagnosticFramePhase::SurfaceAcquisition,
                    surface_started.elapsed(),
                );
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
        if self.diagnostics.latency_active() && automation_surface_texture.is_none() {
            self.diagnostics.latency(LatencyEvent::Phase(
                InteractionTraceStage::SurfaceAcquired,
                surface_started.elapsed(),
            ));
        }
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::SurfaceAcquisition,
            surface_started.elapsed(),
        );
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
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::EguiBuffers,
            egui_buffers_started.elapsed(),
        );
        if self.diagnostics.latency_active() {
            self.diagnostics.latency(LatencyEvent::Phase(
                InteractionTraceStage::EguiBuffersFinished,
                egui_buffers_started.elapsed(),
            ));
        }
        let scale_factor = self.window.scale_factor() as f32;
        let targets = frame_target_plan(capture_texture.is_some());
        let mut alignment_renderer_failures = Vec::new();
        if targets.render_surface {
            alignment_renderer_failures.extend(self.render_composed_frame_target(
                &mut encoder,
                &surface_view,
                &paint_jobs,
                &screen_descriptor,
                scale_factor,
            ));
        }
        if targets.render_capture
            && let Some(texture) = capture_texture.as_ref()
        {
            let capture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            alignment_renderer_failures.extend(self.render_composed_frame_target(
                &mut encoder,
                &capture_view,
                &paint_jobs,
                &screen_descriptor,
                scale_factor,
            ));
        }
        alignment_renderer_failures.dedup();
        for failure in alignment_renderer_failures {
            let name = if failure.message.contains("pinned GPU") {
                "pinned_page_missing"
            } else {
                "render_failed"
            };
            let message = failure.message.clone();
            let fields = object_fields(serde_json::json!({
                "lane": failure.lane,
                "error": failure.message,
                "committed_target_revision": self.alignment_scene_publication_authority.target_revision,
                "desired_target_revision": self.alignment_scene_publication_authority.desired_target_revision(),
                "scroll_cache_authority": self.manual_scroll_cache_diagnostic_state(self.session.panels.active().id),
            }));
            self.queue_manual_diagnostic_trace_failure(
                "alignment_renderer",
                name,
                &message,
                fields,
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
            if !self.refresh_panel_scroll_display(self.session.panels.active().id) {
                self.mark_display_dirty("explicit display invalidation");
                self.window.request_redraw();
            }
        }
        if self.diagnostics.latency_active() {
            self.diagnostics.latency(LatencyEvent::Phase(
                InteractionTraceStage::QueueSubmitted,
                queue_started.elapsed(),
            ));
        }
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::QueueSubmission,
            queue_started.elapsed(),
        );
        let present_started = Instant::now();
        if let Some(surface_frame) = surface_frame {
            surface_frame.present();
        }
        if self.diagnostics.latency_active() && automation_surface_texture.is_none() {
            self.diagnostics.latency(LatencyEvent::Phase(
                InteractionTraceStage::PresentCalled,
                present_started.elapsed(),
            ));
        }
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::Present,
            present_started.elapsed(),
        );
        if let (Some(task), Some((buffer, padded_bytes_per_row))) = (capture_task, capture_readback)
        {
            let pixels = self.read_native_window_capture(&buffer, padded_bytes_per_row);
            self.complete_native_window_capture(task, pixels);
        }
        let frame_ms = frame_started.elapsed().as_secs_f64() * 1_000.0;
        self.diagnostics
            .latency(LatencyEvent::FrameFinished(frame_started.elapsed()));
        self.smoothed_frame_ms = self.smoothed_frame_ms * 0.9 + frame_ms * 0.1;
        let navigation_frame = self
            .automation
            .as_ref()
            .and_then(AutomationRunner::pending_navigation_target)
            .and_then(|(panel_id, target)| {
                let panel = self.session.panels.get(panel_id)?;
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
        // On a lost device the map callback may never fire, so an indefinite poll hangs, and
        // `get_mapped_range` aborts on a buffer that was never mapped. Refuse instead.
        native_capture_device_check(self.gpu_device_health.is_lost())?;
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
        // The device can be lost between the submit and here; the mapping is then not usable.
        native_capture_device_check(self.gpu_device_health.is_lost())?;
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

/// Refuses a native-window readback on a lost GPU device.
fn native_capture_device_check(device_lost: bool) -> std::result::Result<(), String> {
    if device_lost {
        Err(
            "The GPU device was lost; the window capture was abandoned. Retry once rendering \
             has recovered."
                .to_owned(),
        )
    } else {
        Ok(())
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
    use std::time::Instant;

    use super::{
        GPU_RECOVERY_RETRY_INTERVAL, adapter_request_plan, choose_surface_format,
        decoded_capture_pixels, frame_target_plan, gpu_recovery_attempt_is_due, gpu_recovery_wake,
        native_capture_device_check, required_gpu_limits,
    };

    #[test]
    fn adapter_fallback_chain_ends_with_the_software_rasteriser() {
        use wgpu::PowerPreference::{HighPerformance, LowPower, None as Unspecified};
        let plan = |requested| {
            adapter_request_plan(requested)
                .into_iter()
                .map(|request| (request.power_preference, request.force_fallback_adapter))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            plan(None),
            vec![
                (HighPerformance, false),
                (LowPower, false),
                (Unspecified, false),
                (Unspecified, true),
            ]
        );
        assert_eq!(plan(Some(HighPerformance)), plan(None));
        assert_eq!(
            plan(Some(LowPower)),
            vec![
                (LowPower, false),
                (HighPerformance, false),
                (Unspecified, false),
                (Unspecified, true),
            ]
        );
        assert_eq!(
            plan(Some(Unspecified)),
            vec![
                (Unspecified, false),
                (HighPerformance, false),
                (Unspecified, true),
            ]
        );
    }

    #[test]
    fn required_limits_are_downlevel_with_the_adapters_texture_resolution() {
        let adapter = wgpu::Limits {
            max_texture_dimension_2d: 16_384,
            ..wgpu::Limits::downlevel_defaults()
        };
        let required = required_gpu_limits(adapter);
        assert_eq!(required.max_texture_dimension_2d, 16_384);
        let downlevel = wgpu::Limits::downlevel_defaults();
        assert_eq!(
            required.max_uniform_buffer_binding_size,
            downlevel.max_uniform_buffer_binding_size
        );
        assert_eq!(
            required.max_storage_buffers_per_shader_stage,
            downlevel.max_storage_buffers_per_shader_stage
        );
        // A software adapter that only offers the downlevel limits satisfies the request.
        assert!(required_gpu_limits(downlevel.clone()).check_limits(&downlevel));
    }

    #[test]
    fn surface_format_policy_prefers_non_srgb_eight_bit_targets() {
        use wgpu::TextureFormat::{
            Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb, Rgba16Float,
        };
        // Metal and Vulkan commonly list the sRGB format first.
        assert_eq!(
            choose_surface_format(&[Bgra8UnormSrgb, Bgra8Unorm, Rgba16Float]),
            Some(Bgra8Unorm)
        );
        assert_eq!(
            choose_surface_format(&[Rgba8UnormSrgb, Rgba8Unorm]),
            Some(Rgba8Unorm)
        );
        assert_eq!(
            choose_surface_format(&[Rgba16Float, Rgba8UnormSrgb, Bgra8UnormSrgb]),
            Some(Rgba8UnormSrgb)
        );
        assert_eq!(choose_surface_format(&[Rgba16Float]), Some(Rgba16Float));
        assert_eq!(choose_surface_format(&[]), None);
        // The policy agrees with egui-wgpu whenever a non-sRGB 8-bit format is offered.
        for formats in [
            vec![Bgra8UnormSrgb, Bgra8Unorm],
            vec![Rgba8Unorm, Bgra8Unorm],
            vec![Rgba16Float, Rgba8Unorm],
        ] {
            assert_eq!(
                choose_surface_format(&formats),
                egui_wgpu::preferred_framebuffer_format(&formats).ok()
            );
        }
    }

    #[test]
    fn a_lost_device_schedules_its_own_retry_without_user_input() {
        let now = Instant::now();
        assert_eq!(gpu_recovery_wake(false, None, now), None);
        assert_eq!(gpu_recovery_wake(false, Some(now), now), None);
        // Lost and never attempted: redraw at once.
        assert_eq!(gpu_recovery_wake(true, None, now), Some(now));
        // A failed attempt is retried exactly when the rate limit next allows one.
        let retry = gpu_recovery_wake(true, Some(now), now).unwrap();
        assert_eq!(retry, now + GPU_RECOVERY_RETRY_INTERVAL);
        assert!(gpu_recovery_attempt_is_due(Some(now), retry));
    }

    #[test]
    fn native_capture_is_refused_on_a_lost_device() {
        assert!(native_capture_device_check(false).is_ok());
        let error = native_capture_device_check(true).unwrap_err();
        assert!(error.contains("GPU device was lost"), "{error}");
    }

    #[test]
    fn gpu_recovery_attempts_are_rate_limited() {
        let now = Instant::now();
        assert!(gpu_recovery_attempt_is_due(None, now));
        assert!(!gpu_recovery_attempt_is_due(Some(now), now));
        assert!(!gpu_recovery_attempt_is_due(
            Some(now),
            now + GPU_RECOVERY_RETRY_INTERVAL / 2
        ));
        assert!(gpu_recovery_attempt_is_due(
            Some(now),
            now + GPU_RECOVERY_RETRY_INTERVAL
        ));
    }

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
