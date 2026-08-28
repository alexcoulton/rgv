use std::{
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context as _, Result};
use clap::Parser;
use rgv_core::{ContigId, GenomicInterval, ReadStackingMode};
use rgv_hts::{
    AlignmentBatch, AlignmentFlags, AlignmentOpenOptions, AlignmentQuery, AlignmentRecord,
    AlignmentSource as _, CigarOp, CigarOpKind, IndividualReadStatus, LocalAlignmentSource,
    PileupSummary, QueryDetail, QueryOptions, QueryStats, SequenceData,
};
use rgv_layout::{
    AlignmentLayoutOptions, AlignmentRowHistory, AlignmentRowReconciliationStats,
    reconcile_alignment_rows_with_horizon,
};
use serde::Serialize;

const ACTB_CONTIG: &str = "chr7";
const ACTB_VISIBLE_START: u64 = 5_528_398;
const ACTB_VISIBLE_END: u64 = 5_528_400;
const ACTB_HORIZON_START: u64 = 5_472_257;
const ACTB_HORIZON_END: u64 = 5_570_560;
const COMPLETE_READ_LIMIT: usize = 50_000;

#[derive(Debug, Parser)]
#[command(about = "Stable-row performance qualification for dense alignment loci")]
struct Arguments {
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "1000,10000,25000,50000,100000"
    )]
    sizes: Vec<usize>,
    #[arg(long, default_value_t = 5)]
    iterations: usize,
    #[arg(long)]
    actb_bam: Option<PathBuf>,
    #[arg(long, default_value = ACTB_CONTIG)]
    actb_contig: String,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    enforce_gates: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    iterations: usize,
    synthetic: Vec<DenseLayoutResult>,
    actb: Option<ActbResult>,
}

#[derive(Debug, Serialize)]
struct DenseLayoutResult {
    records: usize,
    total_micros: Vec<u64>,
    allocation_micros: Vec<u64>,
    history_index_micros: Vec<u64>,
    history_commit_micros: Vec<u64>,
    canonical_metrics_micros: Vec<u64>,
    p95_total_micros: u64,
    p95_allocation_micros: u64,
    maximum_compatibility_probes: usize,
    rows_appended: usize,
    rows_reused: usize,
}

#[derive(Debug, Serialize)]
struct ActbResult {
    bam: PathBuf,
    contig: String,
    visible_start: u64,
    visible_end: u64,
    horizon_start: u64,
    horizon_end: u64,
    visible_query_micros: u64,
    horizon_query_micros: u64,
    visible_eligible: usize,
    visible_records: usize,
    horizon_eligible: usize,
    horizon_records: usize,
    horizon_too_dense: bool,
    reconciliation: DenseLayoutResult,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    anyhow::ensure!(arguments.iterations > 0, "iterations must be positive");
    anyhow::ensure!(!arguments.sizes.is_empty(), "at least one size is required");
    anyhow::ensure!(
        arguments.sizes.iter().all(|size| *size > 0),
        "every size must be positive"
    );

    let synthetic = arguments
        .sizes
        .iter()
        .copied()
        .map(|records| qualify_synthetic(records, arguments.iterations))
        .collect::<Result<Vec<_>>>()?;
    let actb = arguments
        .actb_bam
        .as_deref()
        .map(|bam| qualify_actb(bam, &arguments.actb_contig, arguments.iterations))
        .transpose()?;
    let report = Report {
        iterations: arguments.iterations,
        synthetic,
        actb,
    };
    if arguments.enforce_gates {
        enforce_gates(&report)?;
    }
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
    Ok(())
}

fn qualify_synthetic(records: usize, iterations: usize) -> Result<DenseLayoutResult> {
    let visible = synthetic_dense_query(records)?;
    let horizon = synthetic_refused_horizon(visible.interval, records.saturating_mul(8));
    benchmark_reconciliation(&visible, &horizon, iterations)
}

fn qualify_actb(bam: &Path, contig: &str, iterations: usize) -> Result<ActbResult> {
    let mut source = LocalAlignmentSource::open(bam, &AlignmentOpenOptions::default())
        .with_context(|| format!("failed to open {}", bam.display()))?;
    let contig_id = source
        .reference_dictionary()
        .id_by_name(contig)
        .with_context(|| format!("{contig} is absent from {}", bam.display()))?;
    let query_options = QueryOptions {
        detail: QueryDetail::Layout,
        max_records: NonZeroUsize::new(COMPLETE_READ_LIMIT).unwrap(),
        ..QueryOptions::default()
    };
    let visible_started = Instant::now();
    let visible = source.query(
        GenomicInterval::new(contig_id, ACTB_VISIBLE_START, ACTB_VISIBLE_END)?,
        query_options.clone(),
        &|| true,
    )?;
    let visible_query_micros = elapsed_micros(visible_started);
    let horizon_started = Instant::now();
    let horizon = source.query(
        GenomicInterval::new(contig_id, ACTB_HORIZON_START, ACTB_HORIZON_END)?,
        query_options,
        &|| true,
    )?;
    let horizon_query_micros = elapsed_micros(horizon_started);
    anyhow::ensure!(
        visible.individual_reads_complete(),
        "ACTB foreground was not Complete: {:?}",
        visible.stats.individual_reads
    );
    anyhow::ensure!(
        matches!(
            horizon.stats.individual_reads,
            IndividualReadStatus::TooDense { .. }
        ),
        "ACTB horizon did not exercise Too-dense fallback: {:?}",
        horizon.stats.individual_reads
    );
    let reconciliation = benchmark_reconciliation(&visible, &horizon, iterations)?;
    Ok(ActbResult {
        bam: bam.to_path_buf(),
        contig: contig.to_owned(),
        visible_start: ACTB_VISIBLE_START,
        visible_end: ACTB_VISIBLE_END,
        horizon_start: ACTB_HORIZON_START,
        horizon_end: ACTB_HORIZON_END,
        visible_query_micros,
        horizon_query_micros,
        visible_eligible: visible.stats.records_eligible,
        visible_records: visible.batch.len(),
        horizon_eligible: horizon.stats.records_eligible,
        horizon_records: horizon.batch.len(),
        horizon_too_dense: true,
        reconciliation,
    })
}

fn benchmark_reconciliation(
    visible: &AlignmentQuery,
    horizon: &AlignmentQuery,
    iterations: usize,
) -> Result<DenseLayoutResult> {
    let options = AlignmentLayoutOptions {
        max_rows: visible.batch.len().max(1),
        ..AlignmentLayoutOptions::default()
    };
    let mut samples = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let mut history = AlignmentRowHistory::default();
        let (_, stats) = reconcile_alignment_rows_with_horizon(
            visible,
            horizon,
            options,
            &mut history,
            ReadStackingMode::Stable,
            false,
        );
        anyhow::ensure!(
            stats.added == visible.batch.len(),
            "only {} of {} dense representatives were added",
            stats.added,
            visible.batch.len()
        );
        samples.push(stats);
    }
    Ok(summarize(visible.batch.len(), &samples))
}

fn summarize(records: usize, samples: &[AlignmentRowReconciliationStats]) -> DenseLayoutResult {
    let total_micros = samples
        .iter()
        .map(|stats| stats.reconciliation_micros)
        .collect::<Vec<_>>();
    let allocation_micros = samples
        .iter()
        .map(|stats| stats.row_allocation_micros)
        .collect::<Vec<_>>();
    let history_index_micros = samples
        .iter()
        .map(|stats| stats.history_index_micros)
        .collect::<Vec<_>>();
    let history_commit_micros = samples
        .iter()
        .map(|stats| stats.history_commit_micros)
        .collect::<Vec<_>>();
    let canonical_metrics_micros = samples
        .iter()
        .map(|stats| stats.canonical_metrics_micros)
        .collect::<Vec<_>>();
    DenseLayoutResult {
        records,
        p95_total_micros: percentile(&total_micros, 0.95),
        p95_allocation_micros: percentile(&allocation_micros, 0.95),
        maximum_compatibility_probes: samples
            .iter()
            .map(|stats| stats.compatibility_probes)
            .max()
            .unwrap_or_default(),
        rows_appended: samples.last().map_or(0, |stats| stats.rows_appended),
        rows_reused: samples.last().map_or(0, |stats| stats.rows_reused),
        total_micros,
        allocation_micros,
        history_index_micros,
        history_commit_micros,
        canonical_metrics_micros,
    }
}

fn synthetic_dense_query(records: usize) -> Result<AlignmentQuery> {
    let interval = GenomicInterval::new(ContigId::new(0), 0, 200)?;
    let mut batch = AlignmentBatch::default();
    for index in 0..records {
        batch
            .push(&AlignmentRecord {
                contig: interval.contig(),
                start: 50,
                end: 100,
                flags: AlignmentFlags::from_bits(0),
                mapping_quality: Some(60),
                mate_contig: None,
                mate_start: None,
                template_length: 0,
                read_name: format!("dense-{index:06}").into_bytes(),
                cigar: vec![CigarOp {
                    kind: CigarOpKind::Match,
                    len: 50,
                }],
                sequence: SequenceData::NotLoaded,
                auxiliary_fields: Vec::new(),
            })
            .map_err(|_| anyhow::anyhow!("synthetic alignment batch overflowed at {index}"))?;
    }
    Ok(AlignmentQuery {
        interval,
        header: Arc::default(),
        batch,
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            records_seen: records,
            records_eligible: records,
            records_emitted: records,
            ..QueryStats::default()
        },
    })
}

fn synthetic_refused_horizon(interval: GenomicInterval, eligible: usize) -> AlignmentQuery {
    AlignmentQuery {
        interval,
        header: Arc::default(),
        batch: AlignmentBatch::default(),
        pileup: PileupSummary::default(),
        junctions: rgv_hts::JunctionSummary::default(),
        pairs: rgv_hts::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            records_seen: eligible,
            records_eligible: eligible,
            individual_reads: IndividualReadStatus::TooDense {
                limit: COMPLETE_READ_LIMIT,
            },
            ..QueryStats::default()
        },
    }
}

fn enforce_gates(report: &Report) -> Result<()> {
    for result in &report.synthetic {
        anyhow::ensure!(
            result.maximum_compatibility_probes == 0,
            "{}-read empty-horizon clique performed {} compatibility probes",
            result.records,
            result.maximum_compatibility_probes
        );
    }
    if let Some(fifty_thousand) = report
        .synthetic
        .iter()
        .find(|result| result.records == 50_000)
    {
        anyhow::ensure!(
            fifty_thousand.p95_allocation_micros <= 250_000,
            "50k allocation p95 {} µs exceeds 250,000 µs",
            fifty_thousand.p95_allocation_micros
        );
    }
    let mut ordered = report.synthetic.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|result| result.records);
    for pair in ordered.windows(2) {
        if pair[1].records != pair[0].records.saturating_mul(2) {
            continue;
        }
        let slower = pair[1].p95_allocation_micros.max(1) as f64;
        let faster = pair[0].p95_allocation_micros.max(1) as f64;
        anyhow::ensure!(
            slower / faster <= 2.5,
            "allocation scaling {} -> {} records was {:.2}x",
            pair[0].records,
            pair[1].records,
            slower / faster
        );
    }
    if let Some(actb) = &report.actb {
        anyhow::ensure!(
            actb.reconciliation.p95_total_micros <= 1_000_000,
            "ACTB reconciliation p95 {} µs exceeds 1,000,000 µs",
            actb.reconciliation.p95_total_micros
        );
    }
    Ok(())
}

fn percentile(samples: &[u64], percentile: f64) -> u64 {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let index = ((ordered.len().saturating_sub(1) as f64) * percentile).ceil() as usize;
    ordered[index.min(ordered.len().saturating_sub(1))]
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}
