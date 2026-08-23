use std::{
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
use rgv_engine::{AlignmentEngine, AlignmentEngineEvent};
use rgv_hts::{AlignmentOpenOptions, QueryOptions};
use rgv_layout::{
    AlignmentLayout, AlignmentLayoutOptions, alignment_read_name_hash, layout_alignment_query,
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
    let (cold_queries, cold_cache_hits) =
        query_benchmark_tracks(&tracks, per_projection_maximum_alignments)?;
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
        let (queries, iteration_cache_hits) =
            query_benchmark_tracks(&tracks, per_projection_maximum_alignments)?;
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
                    track.engine.try_query(
                        request,
                        panel.clock.clone(),
                        panel.interval,
                        query_options_for_panel(panel, per_projection_maximum_alignments),
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
            Some(AlignmentEngineEvent::Opened { dictionary }) => return Ok(dictionary),
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
