use std::{
    collections::HashMap,
    fmt::Write as _,
    fs::{self, File},
    io::{BufReader, Read},
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use clap::Parser;
use rgv_core::{
    DetailLevel, GenerationClock, GenomicInterval, PanelGeneration, PanelId, ReferenceDictionary,
    Viewport,
};
use rgv_engine::{
    AlignmentBrowsingPolicy, AlignmentEngine, AlignmentEngineEvent, AlignmentWorkingSetStats,
};
use rgv_hts::{AlignmentOpenOptions, QueryOptions};
use rgv_layout::{
    AlignmentLayout, AlignmentLayoutOptions, AlignmentRowLayout, AlignmentRowScene,
    alignment_read_name_hash, assign_alignment_rows, layout_alignment_query,
    layout_alignment_query_with_rows,
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

#[derive(Debug, Parser)]
#[command(about = "Headless end-to-end RGV interaction benchmark")]
struct Arguments {
    #[arg(default_value = "benchmark-data/mixed.bam")]
    input: PathBuf,
    #[arg(long, default_value = "chr1:900001-920000")]
    locus: String,
    /// Additional side-by-side panel locus. May be repeated up to 15 times.
    #[arg(long = "panel-locus")]
    panel_loci: Vec<String>,
    #[arg(long, default_value_t = 1_280)]
    viewport_width: usize,
    #[arg(long, default_value_t = 50_000)]
    max_alignments: usize,
    /// Number of synchronized alignment controllers sharing the workspace budgets.
    #[arg(long, default_value_t = 1)]
    track_count: usize,
    /// Workspace-wide decoded-alignment cache budget in MiB.
    #[arg(long, default_value_t = 320)]
    decoded_cache_mib: usize,
    #[arg(long, default_value_t = 10)]
    iterations: usize,
    /// Desired decoded-read margin on each side, as a viewport percentage.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u16).range(0..=400))]
    read_browsing_buffer_percent: u16,
    /// Run narrow/broad/return and bidirectional-pan working-set qualification.
    #[arg(long)]
    working_set_navigation: bool,
    #[arg(long)]
    view_as_pairs: bool,
    #[arg(long)]
    output: Option<PathBuf>,
    /// Fail when median end-to-end latency exceeds this budget.
    #[arg(long)]
    maximum_median_ms: Option<f64>,
    /// Fail when p95 cached end-to-end latency exceeds this budget.
    #[arg(long)]
    maximum_p95_ms: Option<f64>,
    /// Fail when median layout latency exceeds this budget.
    #[arg(long)]
    maximum_layout_ms: Option<f64>,
    /// Fail when the cold query plus layout exceeds this first-use budget.
    #[arg(long)]
    maximum_first_useful_ms: Option<f64>,
    /// Fail when the operating system cannot provide resident-memory samples.
    #[arg(long)]
    require_rss: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    input: PathBuf,
    locus: String,
    loci: Vec<String>,
    input_bytes: u64,
    input_sha256: String,
    viewport_width: usize,
    panel_width: usize,
    maximum_alignments: usize,
    track_count: usize,
    panel_count: usize,
    projection_count: usize,
    per_track_maximum_alignments: usize,
    per_projection_maximum_alignments: usize,
    decoded_cache_mib: usize,
    read_browsing_buffer_percent: u16,
    view_as_pairs: bool,
    open_ms: f64,
    first_useful_ms: f64,
    cold_query_ms: f64,
    query_ms: Vec<f64>,
    layout_ms: Vec<f64>,
    total_ms: Vec<f64>,
    median_query_ms: f64,
    median_layout_ms: f64,
    median_total_ms: f64,
    p95_total_ms: f64,
    density_relayout_ms: Vec<f64>,
    median_density_relayout_ms: f64,
    p95_density_relayout_ms: f64,
    selected_to_top_relayout_ms: Vec<f64>,
    median_selected_to_top_relayout_ms: f64,
    p95_selected_to_top_relayout_ms: f64,
    cache_hits: usize,
    sampled_rss_mib: Vec<f64>,
    records_seen: usize,
    records_retained: usize,
    rendered_alignments: usize,
    primitives: usize,
    maximum_coverage: u32,
    modification_series: usize,
    modified_calls: usize,
    modification_tagged_records: usize,
    junctions_aggregated: usize,
    junctions_rendered: usize,
    maximum_junction_depth: u32,
    junction_spliced_records: usize,
    junction_alignment_strand_fallback_observations: usize,
    junctions_dropped: usize,
    pair_eligible_proper_templates: usize,
    pair_sampled_templates: usize,
    pair_median_template_length: Option<u64>,
    pair_inferred_minimum: Option<u64>,
    pair_inferred_maximum: Option<u64>,
    workspace_records_seen: usize,
    workspace_records_retained: usize,
    workspace_rendered_alignments: usize,
    workspace_primitives: usize,
    working_set_navigation: Option<WorkingSetNavigationReport>,
}

#[derive(Debug, Serialize)]
struct WorkingSetNavigationReport {
    buffer_percent: u16,
    stages: Vec<WorkingSetNavigationStage>,
    source_queries: u64,
    source_queries_avoided: u64,
    decoded_bytes_avoided: u64,
    final_resident_bytes: usize,
    final_cache_bytes: usize,
}

#[derive(Debug, Serialize)]
struct WorkingSetNavigationStage {
    name: String,
    start: u64,
    end: u64,
    foreground_ms: f64,
    foreground_cache_outcome: String,
    foreground_source_queries: u64,
    buffer_settled_ms: f64,
    layout_ms: f64,
    row_scene_hit: bool,
    row_scene_path: String,
    row_scene_projection_ms: f64,
    row_scene_extension_ms: f64,
    row_scene_query_projection_ms: f64,
    resident_scene_prepare_ms: f64,
    render_list_cache_hit: bool,
    render_list_ms: f64,
    cache_outcome: String,
    lookup_micros: u64,
    projection_micros: u64,
    summary_micros: u64,
    summary_tile_hits: usize,
    summary_index_bytes: usize,
    resident_start: Option<u64>,
    resident_end: Option<u64>,
    resident_records: usize,
    resident_bytes: usize,
    cache_bytes: usize,
    cache_maximum_bytes: usize,
    requested_margin_left: u64,
    requested_margin_right: u64,
    effective_margin_left: u64,
    effective_margin_right: u64,
    clamp_reason: Option<String>,
    extensions_requested: u64,
    extensions_completed: u64,
    extensions_cancelled: u64,
    extensions_too_dense: u64,
    extensions_failed: u64,
    source_queries: u64,
    source_queries_avoided: u64,
    decoded_bytes_avoided: u64,
    speculative_remote_bytes: u64,
    speculative_remote_requests: u64,
}

struct BenchmarkTrack {
    engine: AlignmentEngine,
    panels: Vec<BenchmarkPanel>,
}

struct BenchmarkPanel {
    id: PanelId,
    clock: GenerationClock,
    interval: GenomicInterval,
    viewport: Viewport,
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let loci = std::iter::once(arguments.locus.clone())
        .chain(arguments.panel_loci.iter().cloned())
        .collect::<Vec<_>>();
    let panel_count = loci.len();
    anyhow::ensure!(arguments.iterations > 0, "iterations must be positive");
    anyhow::ensure!(
        arguments.viewport_width > 0,
        "viewport width must be positive"
    );
    anyhow::ensure!(
        arguments.max_alignments > 0,
        "alignment cap must be positive"
    );
    anyhow::ensure!(
        (1..=64).contains(&arguments.track_count),
        "track count must be between 1 and 64"
    );
    anyhow::ensure!(
        (1..=16).contains(&panel_count),
        "panel count must be between 1 and 16"
    );
    anyhow::ensure!(
        arguments.viewport_width >= panel_count,
        "workspace viewport must provide at least one pixel per panel"
    );
    let projection_count = arguments
        .track_count
        .checked_mul(panel_count)
        .context("track/panel projection count overflowed")?;
    anyhow::ensure!(
        arguments.max_alignments >= projection_count,
        "workspace alignment cap must provide at least one record per track/panel projection"
    );
    let decoded_cache_bytes = arguments
        .decoded_cache_mib
        .checked_mul(1024 * 1024)
        .context("decoded cache budget is too large")?;
    anyhow::ensure!(
        decoded_cache_bytes >= arguments.track_count,
        "decoded cache budget must provide at least one byte per track"
    );
    let per_track_maximum_alignments = arguments.max_alignments / arguments.track_count;
    let per_projection_maximum_alignments = arguments.max_alignments / projection_count;
    let panel_width = arguments.viewport_width / panel_count;
    let cache_bytes_per_track = NonZeroUsize::new(decoded_cache_bytes / arguments.track_count)
        .context("decoded cache share is zero")?;
    let (tracks, open_ms) = open_benchmark_tracks(
        &arguments.input,
        &loci,
        panel_width,
        arguments.track_count,
        cache_bytes_per_track,
    )?;

    let cold_started = Instant::now();
    let (cold_queries, cold_cache_hits) = query_benchmark_tracks(
        &tracks,
        per_projection_maximum_alignments,
        arguments.read_browsing_buffer_percent,
    )?;
    anyhow::ensure!(
        cold_cache_hits == 0,
        "first query unexpectedly came from cache"
    );
    let cold_query_ms = milliseconds(cold_started);
    let cold_layout_started = Instant::now();
    let cold_layouts = layout_benchmark_tracks(&tracks, &cold_queries, arguments.view_as_pairs);
    let first_useful_ms = cold_query_ms + milliseconds(cold_layout_started);

    let mut query_ms = Vec::with_capacity(arguments.iterations);
    let mut layout_ms = Vec::with_capacity(arguments.iterations);
    let mut total_ms = Vec::with_capacity(arguments.iterations);
    let mut sampled_rss_mib = Vec::with_capacity(arguments.iterations);
    let mut final_queries = cold_queries;
    let mut final_layouts = cold_layouts;
    let mut cache_hits = 0_usize;
    for _ in 0..arguments.iterations {
        let total_started = Instant::now();
        let query_started = Instant::now();
        let (queries, iteration_cache_hits) = query_benchmark_tracks(
            &tracks,
            per_projection_maximum_alignments,
            arguments.read_browsing_buffer_percent,
        )?;
        cache_hits = cache_hits.saturating_add(iteration_cache_hits);
        query_ms.push(milliseconds(query_started));

        let layout_started = Instant::now();
        let layouts = layout_benchmark_tracks(&tracks, &queries, arguments.view_as_pairs);
        layout_ms.push(milliseconds(layout_started));
        total_ms.push(milliseconds(total_started));
        if let Some(rss) = sampled_rss_mib_now() {
            sampled_rss_mib.push(rss);
        }
        final_queries = queries;
        final_layouts = layouts;
    }
    let final_query = final_queries
        .first()
        .and_then(|queries| queries.first())
        .context("benchmark produced no query projection")?;
    let final_layout = final_layouts
        .first()
        .and_then(|layouts| layouts.first())
        .context("benchmark produced no layout projection")?;
    let query_stats = final_query.stats;
    let layout_stats = final_layout.stats;
    let modification_counts = modification_counts(final_query);
    let junction_counts = junction_counts(final_query);
    let pair_summary = final_query.pairs;
    let workspace_records_seen = final_queries
        .iter()
        .flatten()
        .map(|query| query.stats.records_seen)
        .sum();
    let workspace_records_retained = final_queries
        .iter()
        .flatten()
        .map(|query| query.stats.records_emitted)
        .sum();
    let workspace_rendered_alignments = final_layouts
        .iter()
        .flatten()
        .map(|layout| layout.stats.rendered_alignments)
        .sum();
    let workspace_primitives = final_layouts
        .iter()
        .flatten()
        .map(|layout| layout.stats.primitive_count)
        .sum();
    let density_relayout_ms = benchmark_layout_variation(
        &tracks,
        &final_queries,
        arguments.view_as_pairs,
        arguments.iterations,
        |_, options| {
            options.row_height = 4.0;
            options.read_height = 3.0;
        },
    );
    let selected_to_top_relayout_ms = benchmark_layout_variation(
        &tracks,
        &final_queries,
        arguments.view_as_pairs,
        arguments.iterations,
        |query, options| {
            options.promoted_read_name_hash = query
                .batch
                .len()
                .checked_sub(1)
                .and_then(|index| query.batch.read_name(index))
                .map(alignment_read_name_hash);
        },
    );
    let working_set_navigation = arguments
        .working_set_navigation
        .then(|| {
            benchmark_working_set_navigation(
                tracks
                    .first()
                    .context("working-set benchmark has no track")?,
                per_projection_maximum_alignments,
                arguments.read_browsing_buffer_percent,
                arguments.view_as_pairs,
            )
        })
        .transpose()?;
    let report = Report {
        input: arguments.input.clone(),
        locus: arguments.locus,
        loci,
        input_bytes: fs::metadata(&arguments.input)?.len(),
        input_sha256: sha256(&arguments.input)?,
        viewport_width: arguments.viewport_width,
        panel_width,
        maximum_alignments: arguments.max_alignments,
        track_count: arguments.track_count,
        panel_count,
        projection_count,
        per_track_maximum_alignments,
        per_projection_maximum_alignments,
        decoded_cache_mib: arguments.decoded_cache_mib,
        read_browsing_buffer_percent: arguments.read_browsing_buffer_percent,
        view_as_pairs: arguments.view_as_pairs,
        open_ms,
        first_useful_ms,
        cold_query_ms,
        median_query_ms: median(&query_ms),
        median_layout_ms: median(&layout_ms),
        median_total_ms: median(&total_ms),
        p95_total_ms: percentile(&total_ms, 0.95),
        median_density_relayout_ms: median(&density_relayout_ms),
        p95_density_relayout_ms: percentile(&density_relayout_ms, 0.95),
        median_selected_to_top_relayout_ms: median(&selected_to_top_relayout_ms),
        p95_selected_to_top_relayout_ms: percentile(&selected_to_top_relayout_ms, 0.95),
        density_relayout_ms,
        selected_to_top_relayout_ms,
        cache_hits,
        sampled_rss_mib,
        query_ms,
        layout_ms,
        total_ms,
        records_seen: query_stats.records_seen,
        records_retained: query_stats.records_emitted,
        rendered_alignments: layout_stats.rendered_alignments,
        primitives: layout_stats.primitive_count,
        maximum_coverage: layout_stats.maximum_coverage,
        modification_series: modification_counts.0,
        modified_calls: modification_counts.1,
        modification_tagged_records: modification_counts.2,
        junctions_aggregated: junction_counts.0,
        junctions_rendered: layout_stats.rendered_junctions,
        maximum_junction_depth: layout_stats.maximum_junction_depth,
        junction_spliced_records: junction_counts.1,
        junction_alignment_strand_fallback_observations: junction_counts.2,
        junctions_dropped: junction_counts.3,
        pair_eligible_proper_templates: pair_summary.eligible_proper_templates,
        pair_sampled_templates: pair_summary.sampled_templates,
        pair_median_template_length: pair_summary.median_template_length,
        pair_inferred_minimum: pair_summary
            .inferred_bounds
            .and_then(|bounds| bounds.minimum),
        pair_inferred_maximum: pair_summary
            .inferred_bounds
            .and_then(|bounds| bounds.maximum),
        workspace_records_seen,
        workspace_records_retained,
        workspace_rendered_alignments,
        workspace_primitives,
        working_set_navigation,
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(output) = arguments.output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&output, format!("{json}\n"))?;
        println!("wrote {}", output.display());
    } else {
        println!("{json}");
    }
    if let Some(maximum) = arguments.maximum_median_ms {
        anyhow::ensure!(
            report.median_total_ms <= maximum,
            "median total latency {:.3} ms exceeds {:.3} ms budget",
            report.median_total_ms,
            maximum
        );
    }
    if let Some(maximum) = arguments.maximum_layout_ms {
        anyhow::ensure!(
            report.median_layout_ms <= maximum,
            "median layout latency {:.3} ms exceeds {:.3} ms budget",
            report.median_layout_ms,
            maximum
        );
    }
    if let Some(maximum) = arguments.maximum_p95_ms {
        anyhow::ensure!(
            report.p95_total_ms <= maximum,
            "p95 total latency {:.3} ms exceeds {:.3} ms budget",
            report.p95_total_ms,
            maximum
        );
    }
    if let Some(maximum) = arguments.maximum_first_useful_ms {
        anyhow::ensure!(
            report.first_useful_ms <= maximum,
            "first useful latency {:.3} ms exceeds {:.3} ms budget",
            report.first_useful_ms,
            maximum
        );
    }
    anyhow::ensure!(
        report.cache_hits == arguments.iterations.saturating_mul(projection_count),
        "only {} of {} repeated track/panel interactions hit the decoded cache",
        report.cache_hits,
        arguments.iterations.saturating_mul(projection_count)
    );
    if arguments.require_rss {
        anyhow::ensure!(
            report.sampled_rss_mib.len() == arguments.iterations,
            "resident-memory sampling is unavailable or incomplete on this runner"
        );
    }
    Ok(())
}

fn open_benchmark_tracks(
    input: &Path,
    loci: &[String],
    panel_width: usize,
    track_count: usize,
    cache_bytes_per_track: NonZeroUsize,
) -> Result<(Vec<BenchmarkTrack>, f64)> {
    let opened_at = Instant::now();
    let pending = (0..track_count)
        .map(|_| {
            let engine = AlignmentEngine::spawn_with_cache_budget(4, cache_bytes_per_track);
            engine.try_open(input.to_path_buf(), AlignmentOpenOptions::default())?;
            Ok(engine)
        })
        .collect::<Result<Vec<_>>>()?;
    let tracks = pending
        .into_iter()
        .map(|engine| {
            let dictionary = wait_for_open(&engine)?;
            let panels = loci
                .iter()
                .enumerate()
                .map(|(index, locus)| {
                    let interval = parse_locus(&dictionary, locus)?;
                    Ok(BenchmarkPanel {
                        id: PanelId::new(u64::try_from(index).unwrap().saturating_add(1)),
                        clock: GenerationClock::default(),
                        interval,
                        viewport: Viewport::new(interval, panel_width as f64)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(BenchmarkTrack { engine, panels })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((tracks, milliseconds(opened_at)))
}

fn query_benchmark_tracks(
    tracks: &[BenchmarkTrack],
    per_projection_maximum_alignments: usize,
    read_browsing_buffer_percent: u16,
) -> Result<(Vec<Vec<Arc<rgv_hts::AlignmentQuery>>>, usize)> {
    let mut cache_hits = 0_usize;
    let queries = tracks
        .iter()
        .map(|track| {
            let requests = track
                .panels
                .iter()
                .map(|panel| {
                    let request = PanelGeneration::new(panel.id, panel.clock.advance());
                    track.engine.try_query_with_browsing(
                        request,
                        panel.clock.clone(),
                        panel.interval,
                        query_options_for_panel(panel, per_projection_maximum_alignments),
                        AlignmentBrowsingPolicy {
                            buffer_percent: read_browsing_buffer_percent,
                        },
                    )?;
                    Ok(request)
                })
                .collect::<Result<Vec<_>>>()?;
            let completed = wait_for_queries(&track.engine, &requests)?;
            cache_hits = cache_hits
                .saturating_add(completed.iter().filter(|(_, cache_hit)| *cache_hit).count());
            Ok(completed.into_iter().map(|(query, _)| query).collect())
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((queries, cache_hits))
}

fn query_options_for_panel(
    panel: &BenchmarkPanel,
    per_projection_maximum_alignments: usize,
) -> QueryOptions {
    let detail = panel.viewport.detail_level();
    QueryOptions {
        max_records: NonZeroUsize::new(per_projection_maximum_alignments).unwrap(),
        coverage_bins: NonZeroUsize::new(
            (panel.viewport.pixel_width().round() as usize)
                .min(usize::try_from(panel.interval.len()).unwrap_or(usize::MAX))
                .max(1),
        )
        .unwrap(),
        collect_alleles: detail == DetailLevel::Bases,
        collect_modifications: detail != DetailLevel::Overview,
        ..QueryOptions::default()
    }
}

fn benchmark_working_set_navigation(
    track: &BenchmarkTrack,
    maximum_alignments: usize,
    buffer_percent: u16,
    view_as_pairs: bool,
) -> Result<WorkingSetNavigationReport> {
    let panel = track
        .panels
        .first()
        .context("working-set benchmark has no panel")?;
    track.engine.try_clear_cache()?;
    let mut source_query_baseline = 0_u64;
    loop {
        match track.engine.receive_timeout(Duration::from_secs(30))? {
            Some(AlignmentEngineEvent::WorkingSetUpdated { working_set, .. }) => {
                source_query_baseline = source_query_baseline.max(working_set.source_queries);
            }
            Some(AlignmentEngineEvent::CacheCleared { .. }) => break,
            Some(AlignmentEngineEvent::Error { message, .. }) => anyhow::bail!(message),
            Some(_) | None => continue,
        }
    }

    let broad = panel.interval;
    let narrow_length = (broad.len() / 2).max(1);
    let narrow_start = broad
        .start()
        .saturating_add((broad.len().saturating_sub(narrow_length)) / 2);
    let narrow = GenomicInterval::new(
        broad.contig(),
        narrow_start,
        narrow_start.saturating_add(narrow_length),
    )?;
    let pan_step = (narrow.len() / 10).max(1);
    let forward = GenomicInterval::new(
        narrow.contig(),
        narrow.start().saturating_add(pan_step),
        narrow.end().saturating_add(pan_step),
    )?;
    let sequence = [
        ("narrow", narrow),
        ("pan_forward", forward),
        ("pan_backward", narrow),
        ("broad", broad),
        ("narrow_return", narrow),
        ("broad_return", broad),
    ];
    let mut stages = Vec::with_capacity(sequence.len());
    let mut final_working_set = AlignmentWorkingSetStats::default();
    let mut last_settled_source_queries = source_query_baseline;
    let mut final_cache_bytes = 0_usize;
    // Model desktop ownership faithfully: the committed foreground and row
    // horizon remain alive until the next current-generation result replaces
    // them. The engine deliberately keeps only Weak references to these.
    let mut _retained_foreground = None;
    let mut _retained_layout = None;
    let mut row_scenes = Vec::<(AlignmentRowScene, AlignmentLayoutOptions)>::new();
    let mut exact_row_projections =
        HashMap::<(u64, u64), (AlignmentRowLayout, AlignmentLayoutOptions)>::new();
    let mut render_scenes = HashMap::<(u64, u64), Arc<AlignmentLayout>>::new();
    for (name, interval) in sequence {
        let request = PanelGeneration::new(panel.id, panel.clock.advance());
        let viewport = Viewport::new(interval, panel.viewport.pixel_width())?;
        let options = query_options_for_interval(viewport, maximum_alignments);
        let started = Instant::now();
        track.engine.try_query_with_browsing(
            request,
            panel.clock.clone(),
            interval,
            options,
            AlignmentBrowsingPolicy { buffer_percent },
        )?;
        let mut query = None;
        let mut foreground_ms = None;
        let mut foreground_working_set = None;
        let deadline = Instant::now() + Duration::from_secs(120);
        let (working_set, cache_stats, settled_ms, resident_query) = loop {
            anyhow::ensure!(
                Instant::now() < deadline,
                "working-set stage {name} timed out"
            );
            match track.engine.receive_timeout(Duration::from_millis(100))? {
                Some(AlignmentEngineEvent::QueryCompleted {
                    request: seen,
                    query: completed,
                    layout_query,
                    working_set,
                    ..
                }) if seen == request => {
                    foreground_ms = Some(milliseconds(started));
                    _retained_foreground = Some(Arc::clone(&completed));
                    _retained_layout = Some(layout_query);
                    query = Some(completed);
                    foreground_working_set = Some(working_set);
                }
                Some(AlignmentEngineEvent::WorkingSetUpdated {
                    request: seen,
                    resident_query,
                    cache_stats,
                    working_set,
                    ..
                }) if seen == request => {
                    break (
                        working_set,
                        cache_stats,
                        milliseconds(started),
                        resident_query,
                    );
                }
                Some(AlignmentEngineEvent::Error { message, .. }) => anyhow::bail!(message),
                Some(_) | None => continue,
            }
        };
        let query = query.context("working-set update arrived before its foreground")?;
        let foreground_working_set = foreground_working_set
            .context("working-set update arrived without foreground statistics")?;
        let layout_started = Instant::now();
        let layout_query = _retained_layout
            .as_ref()
            .expect("working-set stage retained its layout query");
        let scene_projection_started = Instant::now();
        let mut row_scene_extension_ms = 0.0;
        let mut row_scene_query_projection_ms = 0.0;
        let render_key = (interval.start(), interval.end());
        let mut projected_rows = exact_row_projections
            .get(&render_key)
            .cloned()
            .map(|(layout, options)| {
                (
                    layout,
                    rgv_layout::AlignmentRowSceneProjection::ExactCamera,
                    options,
                )
            })
            .or_else(|| {
                row_scenes
                    .iter()
                    .rev()
                    .find_map(|(scene, options)| {
                        scene.project_physical(&query, options).map(|layout| {
                            (
                                layout,
                                rgv_layout::AlignmentRowSceneProjection::Physical,
                                *options,
                            )
                        })
                    })
                    .or_else(|| {
                        row_scenes.iter().rev().find_map(|(scene, options)| {
                            scene
                                .project_with_outcome(&query, options)
                                .map(|(layout, outcome)| (layout, outcome, *options))
                        })
                    })
            });
        if projected_rows.is_none() {
            let mut extension_options = pair_layout_options(layout_query, view_as_pairs);
            extension_options.max_rows = maximum_alignments.max(1);
            extension_options.materialized_row_range = Some((0, 128));
            if let Some((extended, rows)) = row_scenes.iter().rev().find_map(|(scene, options)| {
                let extension_started = Instant::now();
                let extended = scene.extend_edges(layout_query, &extension_options);
                row_scene_extension_ms += milliseconds(extension_started);
                extended
                    .and_then(|extended| {
                        let projection_started = Instant::now();
                        let rows = extended.project(&query, &extension_options);
                        row_scene_query_projection_ms += milliseconds(projection_started);
                        Some((extended, rows?))
                    })
                    .or_else(|| {
                        // Keep the stored policy as the authoritative comparison;
                        // the explicit options above only stabilize the initial
                        // Complete-read capacity across horizon growth.
                        let extension_started = Instant::now();
                        let extended = scene.extend_edges(layout_query, options);
                        row_scene_extension_ms += milliseconds(extension_started);
                        extended.and_then(|extended| {
                            let projection_started = Instant::now();
                            let rows = extended.project(&query, options);
                            row_scene_query_projection_ms += milliseconds(projection_started);
                            Some((extended, rows?))
                        })
                    })
            }) {
                row_scenes.push((extended, extension_options));
                projected_rows = Some((
                    rows,
                    rgv_layout::AlignmentRowSceneProjection::EdgeExtension,
                    extension_options,
                ));
            }
        }
        let row_scene_hit = projected_rows.is_some();
        let row_scene_path = projected_rows.as_ref().map_or_else(
            || "miss".to_owned(),
            |(_, outcome, _)| format!("{outcome:?}"),
        );
        let row_scene_projection_ms = milliseconds(scene_projection_started);
        let (rows, options) = if let Some((rows, _, options)) = projected_rows {
            (rows, options)
        } else {
            let mut options = pair_layout_options(layout_query, view_as_pairs);
            options.max_rows = maximum_alignments.max(1);
            // Match the desktop's vertically virtualized read pane. Row
            // assignment remains complete, while render-list work covers the
            // visible rows plus a generous overscan rather than an imaginary
            // million-row-tall window.
            options.materialized_row_range = Some((0, 128));
            let horizon_rows = assign_alignment_rows(layout_query, options, None);
            let scene = AlignmentRowScene::new(layout_query, &horizon_rows, &options)
                .context("layout horizon could not form a persistent row scene")?;
            let rows = scene
                .project(&query, &options)
                .context("foreground records were absent from their layout horizon")?;
            row_scenes.push((scene, options));
            (rows, options)
        };
        exact_row_projections.insert(render_key, (rows.clone(), options));
        let render_list_started = Instant::now();
        let (layout, render_list_cache_hit) = render_scenes.get(&render_key).cloned().map_or_else(
            || {
                let layout = Arc::new(layout_alignment_query_with_rows(
                    &query, viewport, options, &rows,
                ));
                render_scenes.insert(render_key, Arc::clone(&layout));
                (layout, false)
            },
            |layout| (layout, true),
        );
        std::hint::black_box(layout);
        let render_list_ms = milliseconds(render_list_started);
        let layout_ms = milliseconds(layout_started);
        let prepare_started = Instant::now();
        if let Some(resident_query) = resident_query
            && let Some((prepared, options)) = row_scenes.last().and_then(|(scene, options)| {
                scene
                    .prepare_resident_extension(&resident_query)
                    .map(|prepared| (prepared, *options))
            })
        {
            row_scenes.push((prepared, options));
        }
        let resident_scene_prepare_ms = milliseconds(prepare_started);
        stages.push(WorkingSetNavigationStage {
            name: name.to_owned(),
            start: interval.start(),
            end: interval.end(),
            foreground_ms: foreground_ms.unwrap_or(settled_ms),
            foreground_cache_outcome: format!("{:?}", foreground_working_set.cache_outcome),
            foreground_source_queries: foreground_working_set
                .source_queries
                .saturating_sub(last_settled_source_queries),
            buffer_settled_ms: settled_ms,
            layout_ms,
            row_scene_hit,
            row_scene_path,
            row_scene_projection_ms,
            row_scene_extension_ms,
            row_scene_query_projection_ms,
            resident_scene_prepare_ms,
            render_list_cache_hit,
            render_list_ms,
            cache_outcome: format!("{:?}", working_set.cache_outcome),
            lookup_micros: working_set.lookup_micros,
            projection_micros: working_set.projection_micros,
            summary_micros: working_set.summary_micros,
            summary_tile_hits: working_set.summary_tile_hits,
            summary_index_bytes: working_set.summary_index_bytes,
            resident_start: working_set.resident_start,
            resident_end: working_set.resident_end,
            resident_records: working_set.resident_records,
            resident_bytes: working_set.resident_bytes,
            cache_bytes: cache_stats.resident_weight,
            cache_maximum_bytes: cache_stats.maximum_weight,
            requested_margin_left: working_set.requested_margin_left,
            requested_margin_right: working_set.requested_margin_right,
            effective_margin_left: working_set.effective_margin_left,
            effective_margin_right: working_set.effective_margin_right,
            clamp_reason: working_set.clamp_reason.map(|reason| format!("{reason:?}")),
            extensions_requested: working_set.extensions_requested,
            extensions_completed: working_set.extensions_completed,
            extensions_cancelled: working_set.extensions_cancelled,
            extensions_too_dense: working_set.extensions_too_dense,
            extensions_failed: working_set.extensions_failed,
            source_queries: working_set
                .source_queries
                .saturating_sub(source_query_baseline),
            source_queries_avoided: working_set.source_queries_avoided,
            decoded_bytes_avoided: working_set.decoded_bytes_avoided,
            speculative_remote_bytes: working_set.speculative_remote_bytes,
            speculative_remote_requests: working_set.speculative_remote_requests,
        });
        final_working_set = working_set;
        last_settled_source_queries = working_set.source_queries;
        final_cache_bytes = cache_stats.resident_weight;
    }
    Ok(WorkingSetNavigationReport {
        buffer_percent,
        source_queries: final_working_set
            .source_queries
            .saturating_sub(source_query_baseline),
        source_queries_avoided: final_working_set.source_queries_avoided,
        decoded_bytes_avoided: final_working_set.decoded_bytes_avoided,
        final_resident_bytes: final_working_set.resident_bytes,
        final_cache_bytes,
        stages,
    })
}

fn query_options_for_interval(viewport: Viewport, maximum_alignments: usize) -> QueryOptions {
    let detail = viewport.detail_level();
    QueryOptions {
        max_records: NonZeroUsize::new(maximum_alignments).unwrap(),
        coverage_bins: NonZeroUsize::new(
            (viewport.pixel_width().round() as usize)
                .min(usize::try_from(viewport.interval().len()).unwrap_or(usize::MAX))
                .max(1),
        )
        .unwrap(),
        collect_alleles: detail == DetailLevel::Bases,
        collect_modifications: detail != DetailLevel::Overview,
        ..QueryOptions::default()
    }
}

fn layout_benchmark_tracks(
    tracks: &[BenchmarkTrack],
    queries: &[Vec<Arc<rgv_hts::AlignmentQuery>>],
    view_as_pairs: bool,
) -> Vec<Vec<AlignmentLayout>> {
    tracks
        .iter()
        .zip(queries)
        .map(|(track, queries)| {
            track
                .panels
                .iter()
                .zip(queries)
                .map(|(panel, query)| {
                    layout_alignment_query(
                        query,
                        panel.viewport,
                        pair_layout_options(query, view_as_pairs),
                    )
                })
                .collect()
        })
        .collect()
}

fn pair_layout_options(
    query: &rgv_hts::AlignmentQuery,
    view_as_pairs: bool,
) -> AlignmentLayoutOptions {
    AlignmentLayoutOptions {
        view_as_pairs,
        pair_length_bounds: query.pairs.inferred_bounds.unwrap_or_default(),
        ..AlignmentLayoutOptions::default()
    }
}

fn benchmark_layout_variation(
    tracks: &[BenchmarkTrack],
    queries: &[Vec<Arc<rgv_hts::AlignmentQuery>>],
    view_as_pairs: bool,
    iterations: usize,
    mut configure: impl FnMut(&rgv_hts::AlignmentQuery, &mut AlignmentLayoutOptions),
) -> Vec<f64> {
    (0..iterations)
        .map(|_| {
            let started = Instant::now();
            for (track, queries) in tracks.iter().zip(queries) {
                for (panel, query) in track.panels.iter().zip(queries) {
                    let mut options = pair_layout_options(query, view_as_pairs);
                    configure(query, &mut options);
                    std::hint::black_box(layout_alignment_query(query, panel.viewport, options));
                }
            }
            milliseconds(started)
        })
        .collect()
}

fn modification_counts(query: &rgv_hts::AlignmentQuery) -> (usize, usize, usize) {
    (
        query.pileup.modifications.series.len(),
        query
            .pileup
            .modifications
            .series
            .iter()
            .flat_map(|series| &series.bins)
            .fold(0_usize, |total, bin| {
                total.saturating_add(bin.modified_calls as usize)
            }),
        query
            .pileup
            .modifications
            .provenance
            .records_with_modification_tags,
    )
}

fn junction_counts(query: &rgv_hts::AlignmentQuery) -> (usize, usize, usize, usize) {
    (
        query.junctions.junctions.len(),
        query.junctions.provenance.spliced_records,
        query
            .junctions
            .provenance
            .alignment_strand_fallback_observations,
        query.junctions.provenance.dropped_distinct_junctions,
    )
}

fn wait_for_open(engine: &AlignmentEngine) -> Result<ReferenceDictionary> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match engine.receive_timeout(Duration::from_millis(100))? {
            Some(AlignmentEngineEvent::Opened { dictionary, .. }) => return Ok(dictionary),
            Some(AlignmentEngineEvent::Error { message, .. }) => anyhow::bail!(message),
            Some(_) | None => {}
        }
    }
    anyhow::bail!("timed out opening the alignment source")
}

fn wait_for_queries(
    engine: &AlignmentEngine,
    expected: &[PanelGeneration],
) -> Result<Vec<(Arc<rgv_hts::AlignmentQuery>, bool)>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut completed = vec![None; expected.len()];
    while Instant::now() < deadline {
        match engine.receive_timeout(Duration::from_millis(100))? {
            Some(AlignmentEngineEvent::QueryCompleted {
                request,
                query,
                cache_hit,
                ..
            }) => {
                if let Some(index) = expected.iter().position(|candidate| *candidate == request) {
                    completed[index] = Some((query, cache_hit));
                    if completed.iter().all(Option::is_some) {
                        return Ok(completed.into_iter().map(Option::unwrap).collect());
                    }
                }
            }
            Some(AlignmentEngineEvent::Error { request, message })
                if request.is_none()
                    || request.is_some_and(|request| expected.contains(&request)) =>
            {
                anyhow::bail!(message)
            }
            Some(_) | None => {}
        }
    }
    anyhow::bail!("timed out waiting for alignment query generations {expected:?}")
}

fn parse_locus(dictionary: &ReferenceDictionary, locus: &str) -> Result<GenomicInterval> {
    let (name, range) = locus
        .split_once(':')
        .with_context(|| format!("locus must use name:start-end, got {locus}"))?;
    let range = range.replace(',', "");
    let (start, end) = range
        .split_once('-')
        .with_context(|| format!("locus must use name:start-end, got {locus}"))?;
    let one_based_start: u64 = start.parse()?;
    let one_based_end: u64 = end.parse()?;
    anyhow::ensure!(
        one_based_start > 0 && one_based_end >= one_based_start,
        "locus coordinates must be one-based and forward"
    );
    let contig_id = dictionary
        .id_by_name(name)
        .with_context(|| format!("unknown contig {name}"))?;
    let contig_length = dictionary
        .get(contig_id)
        .context("reference dictionary is inconsistent")?
        .length();
    Ok(GenomicInterval::bounded(
        contig_id,
        one_based_start - 1,
        one_based_end,
        contig_length,
    )?)
}

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1_000.0
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[cfg(target_os = "linux")]
fn sampled_rss_mib_now() -> Option<f64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()?;
    Some(kib / 1024.0)
}

#[cfg(not(target_os = "linux"))]
#[cfg(not(target_os = "macos"))]
const fn sampled_rss_mib_now() -> Option<f64> {
    None
}

#[cfg(target_os = "macos")]
fn sampled_rss_mib_now() -> Option<f64> {
    let pid = std::process::id().to_string();
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let kib = std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()?;
    Some(kib / 1024.0)
}

fn sha256(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let mut output = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_display_locus_as_half_open_interval() {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000)]);
        let interval = parse_locus(&dictionary, "chr1:101-200").unwrap();
        assert_eq!(interval.start(), 100);
        assert_eq!(interval.end(), 200);
    }

    #[test]
    fn multi_track_arguments_preserve_workspace_wide_budgets() {
        let arguments = Arguments::try_parse_from([
            "rgv-interaction-bench",
            "--track-count",
            "4",
            "--max-alignments",
            "50000",
            "--decoded-cache-mib",
            "320",
            "--panel-locus",
            "chr2:1-200000",
        ])
        .unwrap();

        assert_eq!(arguments.track_count, 4);
        assert_eq!(arguments.panel_loci, vec!["chr2:1-200000"]);
        let panel_count = arguments.panel_loci.len() + 1;
        assert_eq!(arguments.max_alignments / arguments.track_count, 12_500);
        assert_eq!(
            arguments.max_alignments / (arguments.track_count * panel_count),
            6_250
        );
        assert_eq!(arguments.decoded_cache_mib / arguments.track_count, 80);
    }
}
