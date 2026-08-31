use std::{
    collections::{HashMap, HashSet},
    fmt::Write as _,
    fs::{self, File},
    io::{BufReader, Read},
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use clap::Parser;
use rgv_alignment_scene::{
    AlignmentReadLabelMode, AlignmentSceneCamera, AlignmentStyleContract, AlleleSummaryDetail,
    AllocationLedger, CapabilityRequirement, GeometryLod, LayoutContract, PayloadContract,
    PresentationContract, SummaryContract,
};
use rgv_core::{
    AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, GenomicInterval,
    OverlappingMatePolicy, ReadStackingMode, ReferenceDictionary, Viewport,
};
use rgv_engine::{
    AlignmentCoordinatorFrame, AlignmentIndividualReadCapability, AlignmentSceneBuildContract,
    AlignmentSceneCoordinatorTelemetry, AlignmentSceneEngine, AlignmentSceneEngineEvent,
    AlignmentScenePreparationPolicy, AlignmentSceneRequestToken, LocalAlignmentSceneEngineConfig,
    LocalBamCanonicalBackend, SourceOwnerId,
};
use rgv_hts::{AlignmentOpenOptions, QueryDetail, QueryOptions, TemplateLengthBounds};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

const GEOMETRY_TILE_BASES: u64 = 32_768;
const ROW_BLOCK_ROWS: u32 = 256;
const EVENT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Parser)]
#[command(about = "Headless end-to-end unified alignment-scene interaction benchmark")]
struct Arguments {
    #[arg(default_value = "benchmark-data/mixed.bam")]
    input: PathBuf,
    #[arg(long, default_value = "chr1:900001-920000")]
    locus: String,
    #[arg(long = "panel-locus")]
    panel_loci: Vec<String>,
    #[arg(long, default_value_t = 1_280)]
    viewport_width: usize,
    #[arg(long, default_value_t = 10_000_000)]
    max_alignments: usize,
    #[arg(long, default_value_t = 1)]
    track_count: usize,
    #[arg(long, default_value_t = 320)]
    decoded_cache_mib: usize,
    #[arg(long, default_value_t = 10)]
    iterations: usize,
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(0..=400))]
    read_browsing_buffer_percent: u16,
    #[arg(long)]
    working_set_navigation: bool,
    #[arg(long)]
    view_as_pairs: bool,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    maximum_median_ms: Option<f64>,
    #[arg(long)]
    maximum_p95_ms: Option<f64>,
    /// Compatibility spelling: scene-camera publication now replaces the old layout pass.
    #[arg(long)]
    maximum_layout_ms: Option<f64>,
    #[arg(long)]
    maximum_first_useful_ms: Option<f64>,
    #[arg(long)]
    require_rss: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    architecture: &'static str,
    input: PathBuf,
    input_bytes: u64,
    input_sha256: String,
    loci: Vec<String>,
    viewport_width: usize,
    maximum_alignments: usize,
    track_count: usize,
    panel_count: usize,
    projection_count: usize,
    scene_budget_mib: usize,
    read_browsing_buffer_percent: u16,
    view_as_pairs: bool,
    working_set_navigation: bool,
    open_ms: f64,
    first_useful_ms: f64,
    camera_publication_ms: Vec<f64>,
    median_total_ms: f64,
    p95_total_ms: f64,
    sampled_rss_mib: Vec<f64>,
    committed_records: usize,
    committed_rows: usize,
    scene_publications: u64,
    camera_updates: u64,
    camera_only_updates: u64,
    source_acquisitions: u64,
    source_retargets: u64,
    ledger_budget_bytes: usize,
    ledger_charged_bytes: usize,
    ledger_reserved_bytes: usize,
    ledger_transient_high_water_bytes: usize,
    ledger_pinned_bytes: usize,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    anyhow::ensure!(arguments.iterations > 0, "iterations must be positive");
    anyhow::ensure!(
        arguments.viewport_width > 0,
        "viewport width must be positive"
    );
    anyhow::ensure!(arguments.track_count > 0, "track count must be positive");
    anyhow::ensure!(
        arguments.panel_loci.len() < 16,
        "at most 16 panels are supported"
    );
    let maximum_alignments = NonZeroUsize::new(arguments.max_alignments)
        .context("maximum alignments must be positive")?;
    let scene_budget_bytes = arguments
        .decoded_cache_mib
        .checked_mul(1024 * 1024)
        .context("scene budget is too large")?
        .max(1);
    let ledger = AllocationLedger::new_with_transient_budget(
        scene_budget_bytes,
        scene_budget_bytes,
        scene_budget_bytes,
    );
    let mut query_options = QueryOptions {
        detail: QueryDetail::Full,
        layout_requires_sequence: true,
        max_records: maximum_alignments,
        include_reference: true,
        ..QueryOptions::default()
    };
    query_options.collect_alleles = false;
    query_options.collect_modifications = false;

    let opened_at = Instant::now();
    let engine = AlignmentSceneEngine::spawn_local_bam(LocalAlignmentSceneEngineConfig {
        path: arguments.input.clone(),
        open_options: AlignmentOpenOptions::default(),
        query_options,
        ledger: ledger.clone(),
        mailbox_control_capacity: NonZeroUsize::new(256).unwrap(),
        source_control_capacity: NonZeroUsize::new(64).unwrap(),
        maximum_source_control_burst: NonZeroUsize::new(8).unwrap(),
    });
    let metadata = wait_for_source(&engine)?;
    let open_ms = milliseconds(opened_at);
    let dictionary = &metadata.dictionary;
    let locus_labels = std::iter::once(arguments.locus.clone())
        .chain(arguments.panel_loci.iter().cloned())
        .collect::<Vec<_>>();
    let mut viewports = locus_labels
        .iter()
        .map(|locus| {
            Viewport::new(
                parse_locus(dictionary, locus)?,
                arguments.viewport_width as f64 / locus_labels.len() as f64,
            )
            .map_err(anyhow::Error::from)
        })
        .collect::<Result<Vec<_>>>()?;
    let policy = preparation_policy(arguments.read_browsing_buffer_percent)?;

    let first_started = Instant::now();
    let mut expected = HashMap::new();
    for track_index in 0..arguments.track_count {
        for (panel_index, viewport) in viewports.iter().copied().enumerate() {
            let owner = owner_id(track_index, panel_index);
            let request_token = AlignmentSceneRequestToken::new(1);
            let contig = viewport.interval().contig();
            let contig_length = dictionary
                .get(contig)
                .context("reference dictionary is inconsistent")?
                .length();
            engine.try_insert_lane_for_request(
                owner,
                request_token,
                build_contract(&metadata, viewport, contig_length, arguments.view_as_pairs),
                policy,
            )?;
            engine.try_update_camera_for_request(
                owner,
                request_token,
                AlignmentSceneCamera::with_visible_rows(viewport, 0..128)?,
            )?;
            expected.insert(owner, request_token);
        }
    }
    wait_for_frames(&engine, &expected)?;
    let first_useful_ms = milliseconds(first_started);

    let mut samples = Vec::with_capacity(arguments.iterations);
    let mut rss = Vec::new();
    let mut final_frames = HashMap::new();
    let mut telemetry_by_owner = HashMap::new();
    for iteration in 0..arguments.iterations {
        let started = Instant::now();
        for (panel_index, viewport) in viewports.iter_mut().enumerate() {
            let contig_length = dictionary
                .get(viewport.interval().contig())
                .context("reference dictionary is inconsistent")?
                .length();
            let magnitude = if arguments.working_set_navigation {
                16.0
            } else {
                1.0
            };
            let delta = if iteration % 2 == 0 {
                magnitude
            } else {
                -magnitude
            };
            *viewport = viewport.pan_pixels(delta, contig_length)?;
            for track_index in 0..arguments.track_count {
                let owner = owner_id(track_index, panel_index);
                let token = AlignmentSceneRequestToken::new(iteration as u64 + 2);
                engine.try_update_camera_for_request(
                    owner,
                    token,
                    AlignmentSceneCamera::with_visible_rows(*viewport, 0..128)?,
                )?;
            }
        }
        let expected = (0..arguments.track_count)
            .flat_map(|track_index| {
                (0..viewports.len()).map(move |panel_index| {
                    (
                        owner_id(track_index, panel_index),
                        AlignmentSceneRequestToken::new(iteration as u64 + 2),
                    )
                })
            })
            .collect::<HashMap<_, _>>();
        for (owner, (frame, telemetry)) in wait_for_frames(&engine, &expected)? {
            final_frames.insert(owner, frame);
            telemetry_by_owner.insert(owner, telemetry);
        }
        samples.push(milliseconds(started));
        if let Some(value) = sampled_rss_mib_now() {
            rss.push(value);
        }
    }
    if arguments.require_rss {
        anyhow::ensure!(!rss.is_empty(), "resident-memory samples are unavailable");
    }

    let median_total_ms = median(&samples);
    let p95_total_ms = percentile(&samples, 0.95);
    enforce_limit(
        "median scene publication",
        median_total_ms,
        arguments.maximum_median_ms,
    )?;
    enforce_limit(
        "p95 scene publication",
        p95_total_ms,
        arguments.maximum_p95_ms,
    )?;
    enforce_limit(
        "median scene camera projection",
        median_total_ms,
        arguments.maximum_layout_ms,
    )?;
    enforce_limit(
        "first useful scene",
        first_useful_ms,
        arguments.maximum_first_useful_ms,
    )?;

    let mut unique_scenes = HashSet::new();
    let mut committed_records = 0_usize;
    let mut committed_rows = 0_usize;
    for frame in final_frames.values() {
        if let Some(snapshot) = &frame.snapshot
            && unique_scenes.insert(snapshot.revision())
        {
            committed_records =
                committed_records.saturating_add(snapshot.reads().evidence().record_count());
            committed_rows =
                committed_rows.saturating_add(snapshot.reads().atlas().row_count() as usize);
        }
    }
    let telemetry = telemetry_by_owner.values().copied().fold(
        AlignmentSceneCoordinatorTelemetry::default(),
        |mut sum, item| {
            sum.scene_publications = sum
                .scene_publications
                .saturating_add(item.scene_publications);
            sum.camera_updates = sum.camera_updates.saturating_add(item.camera_updates);
            sum.camera_only_updates = sum
                .camera_only_updates
                .saturating_add(item.camera_only_updates);
            sum.source_acquisitions = sum
                .source_acquisitions
                .saturating_add(item.source_acquisitions);
            sum.source_retargets = sum.source_retargets.saturating_add(item.source_retargets);
            sum
        },
    );
    let progress = ledger.progress()?;
    let report = Report {
        architecture: "unified_alignment_scene",
        input_bytes: fs::metadata(&arguments.input)?.len(),
        input_sha256: sha256(&arguments.input)?,
        input: arguments.input,
        loci: locus_labels,
        viewport_width: arguments.viewport_width,
        maximum_alignments: arguments.max_alignments,
        track_count: arguments.track_count,
        panel_count: viewports.len(),
        projection_count: arguments.track_count.saturating_mul(viewports.len()),
        scene_budget_mib: arguments.decoded_cache_mib,
        read_browsing_buffer_percent: arguments.read_browsing_buffer_percent,
        view_as_pairs: arguments.view_as_pairs,
        working_set_navigation: arguments.working_set_navigation,
        open_ms,
        first_useful_ms,
        camera_publication_ms: samples,
        median_total_ms,
        p95_total_ms,
        sampled_rss_mib: rss,
        committed_records,
        committed_rows,
        scene_publications: telemetry.scene_publications,
        camera_updates: telemetry.camera_updates,
        camera_only_updates: telemetry.camera_only_updates,
        source_acquisitions: telemetry.source_acquisitions,
        source_retargets: telemetry.source_retargets,
        ledger_budget_bytes: progress.budget,
        ledger_charged_bytes: progress.charged,
        ledger_reserved_bytes: progress.reserved,
        ledger_transient_high_water_bytes: progress.transient_high_water,
        ledger_pinned_bytes: progress.pinned,
    };
    let json = serde_json::to_string_pretty(&report)?;
    if let Some(output) = arguments.output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, format!("{json}\n"))?;
    }
    println!("{json}");
    Ok(())
}

fn owner_id(track_index: usize, panel_index: usize) -> SourceOwnerId {
    SourceOwnerId::new(
        u64::try_from(track_index)
            .unwrap_or(u64::MAX)
            .saturating_mul(16)
            .saturating_add(u64::try_from(panel_index).unwrap_or(u64::MAX))
            .saturating_add(1),
    )
}

fn preparation_policy(buffer_percent: u16) -> Result<AlignmentScenePreparationPolicy> {
    let side = f64::from(buffer_percent) / 100.0;
    Ok(AlignmentScenePreparationPolicy::new(
        side,
        side,
        side * 0.25,
        128,
        32,
    )?)
}

fn build_contract(
    metadata: &rgv_engine::AlignmentSceneSourceMetadata,
    viewport: Viewport,
    contig_length: u64,
    view_as_pairs: bool,
) -> AlignmentSceneBuildContract {
    AlignmentSceneBuildContract {
        source_revision: metadata.source_revision,
        record_contract: metadata.record_contract,
        contig: viewport.interval().contig(),
        contig_length,
        payload: PayloadContract::new_all_auxiliary(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            None,
        ),
        summaries: SummaryContract {
            overlapping_mate_policy: OverlappingMatePolicy::CountOnce,
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            allele_detail: AlleleSummaryDetail::None,
            collect_modifications: false,
        },
        layout: LayoutContract {
            row_scale_class: 0,
            stacking: ReadStackingMode::Compact,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
        },
        presentation: PresentationContract {
            geometry_lod: GeometryLod::Span,
            style: AlignmentStyleContract {
                read_labels: AlignmentReadLabelMode::Hidden,
                ..AlignmentStyleContract::default()
            },
            reference: None,
            annotation: None,
        },
        geometry_tile_bases: NonZeroU64::new(GEOMETRY_TILE_BASES).unwrap(),
        summary_tile_bases: NonZeroU64::new(GEOMETRY_TILE_BASES).unwrap(),
        row_block_rows: NonZeroU32::new(ROW_BLOCK_ROWS).unwrap(),
        normalization_mapped_records: metadata.normalization_mapped_records,
        reference: None,
        annotation: None,
    }
}

fn wait_for_source(
    engine: &AlignmentSceneEngine<LocalBamCanonicalBackend>,
) -> Result<rgv_engine::AlignmentSceneSourceMetadata> {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while Instant::now() < deadline {
        match engine.receive_timeout(Duration::from_millis(50))? {
            Some(AlignmentSceneEngineEvent::SourceOpened { metadata }) => return Ok(metadata),
            Some(AlignmentSceneEngineEvent::Error { message, .. }) => anyhow::bail!(message),
            Some(_) | None => {}
        }
    }
    anyhow::bail!("timed out opening alignment scene source")
}

fn wait_for_frames(
    engine: &AlignmentSceneEngine<LocalBamCanonicalBackend>,
    expected: &HashMap<SourceOwnerId, AlignmentSceneRequestToken>,
) -> Result<
    HashMap<
        SourceOwnerId,
        (
            AlignmentCoordinatorFrame,
            AlignmentSceneCoordinatorTelemetry,
        ),
    >,
> {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    let mut completed = HashMap::new();
    while completed.len() < expected.len() && Instant::now() < deadline {
        match engine.receive_timeout(Duration::from_millis(50))? {
            Some(AlignmentSceneEngineEvent::FrameUpdated {
                owner,
                request_token,
                frame,
                telemetry,
                ..
            }) if expected.get(&owner) == Some(&request_token)
                && frame.visible_complete()
                && matches!(
                    frame.individual_reads,
                    AlignmentIndividualReadCapability::Complete { .. }
                ) =>
            {
                completed.insert(owner, (frame, telemetry));
            }
            Some(AlignmentSceneEngineEvent::IndividualReadsRefused {
                eligible_records,
                complete_read_limit,
                ..
            }) => anyhow::bail!(
                "complete scene refused {eligible_records} reads at limit {complete_read_limit}"
            ),
            Some(AlignmentSceneEngineEvent::ResourcePressure { pressure, .. }) => {
                anyhow::bail!("alignment scene resource pressure: {pressure}")
            }
            Some(AlignmentSceneEngineEvent::Error { message, .. }) => anyhow::bail!(message),
            Some(_) | None => {}
        }
    }
    anyhow::ensure!(
        completed.len() == expected.len(),
        "timed out waiting for {} unified scene publications (received {})",
        expected.len(),
        completed.len(),
    );
    Ok(completed)
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

fn enforce_limit(label: &str, value: f64, limit: Option<f64>) -> Result<()> {
    if let Some(limit) = limit {
        anyhow::ensure!(
            value <= limit,
            "{label} {value:.2} ms exceeds {limit:.2} ms"
        );
    }
    Ok(())
}

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1_000.0
}

fn median(values: &[f64]) -> f64 {
    percentile(values, 0.5)
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

#[cfg(target_os = "macos")]
fn sampled_rss_mib_now() -> Option<f64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .map(|kib| kib / 1024.0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn sampled_rss_mib_now() -> Option<f64> {
    None
}

fn sha256(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    Ok(output)
}
