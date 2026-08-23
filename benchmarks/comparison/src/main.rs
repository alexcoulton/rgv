use std::{fmt::Write as _, path::PathBuf};

use anyhow::{Context as _, Result};
use clap::Parser;
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(about = "Compile matched RGV and IGV samples into a reproducible comparison")]
struct Arguments {
    rgv_report: PathBuf,
    igv_samples: PathBuf,
    #[arg(long, default_value = "benchmark-results/RGV_VS_IGV.md")]
    output: PathBuf,
    /// Fail unless RGV's interaction p50 and p95 are both lower than IGV's.
    #[arg(long)]
    require_rgv_faster: bool,
    /// Permit fewer than ten samples for development-only reports.
    #[arg(long)]
    allow_development_sample: bool,
    /// Require memory plus matched RGV/IGV remote request, byte, and wall-time samples.
    #[arg(long)]
    require_complete_telemetry: bool,
}

#[derive(Debug, Deserialize)]
struct RgvReport {
    input: PathBuf,
    locus: String,
    input_sha256: String,
    viewport_width: usize,
    maximum_alignments: usize,
    open_ms: f64,
    first_useful_ms: f64,
    sampled_rss_mib: Vec<f64>,
    total_ms: Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct IgvSamples {
    schema_version: u32,
    igv_version: String,
    machine: String,
    fixture_sha256: String,
    locus: String,
    viewport_width: usize,
    maximum_alignments: usize,
    startup_to_first_snapshot_ms: Vec<f64>,
    navigation_to_snapshot_ms: Vec<f64>,
    sampled_rss_mib: Option<Vec<f64>>,
    rgv_remote_requests: Option<Vec<u64>>,
    rgv_remote_response_bytes: Option<Vec<u64>>,
    rgv_remote_wall_ms: Option<Vec<f64>>,
    rgv_cpu_percent: Option<Vec<f64>>,
    remote_requests: Option<Vec<u64>>,
    remote_response_bytes: Option<Vec<u64>>,
    remote_wall_ms: Option<Vec<f64>>,
    cpu_percent: Option<Vec<f64>>,
    commands: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
struct Summary {
    p50: f64,
    p95: f64,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let rgv: RgvReport = serde_json::from_str(&std::fs::read_to_string(&arguments.rgv_report)?)?;
    let igv: IgvSamples = serde_json::from_str(&std::fs::read_to_string(&arguments.igv_samples)?)?;
    validate_inputs(&arguments, &rgv, &igv)?;
    let rgv_interaction = summarize(&rgv.total_ms)?;
    let igv_interaction = summarize(&igv.navigation_to_snapshot_ms)?;
    let igv_startup = summarize(&igv.startup_to_first_snapshot_ms)?;
    let faster = rgv_interaction.p50 < igv_interaction.p50
        && rgv_interaction.p95 < igv_interaction.p95
        && rgv.first_useful_ms < igv_startup.p50;
    if arguments.require_rgv_faster {
        anyhow::ensure!(
            faster,
            "RGV did not beat IGV for first-useful latency and both interaction percentiles"
        );
        if arguments.require_complete_telemetry {
            enforce_comparative_telemetry(&rgv, &igv)?;
        }
    }

    let mut markdown = String::new();
    writeln!(markdown, "# RGV versus IGV reproducible report\n")?;
    writeln!(markdown, "- Machine: {}", igv.machine)?;
    writeln!(markdown, "- IGV version: {}", igv.igv_version)?;
    writeln!(markdown, "- Fixture: `{}`", rgv.input.display())?;
    writeln!(markdown, "- SHA-256: `{}`", rgv.input_sha256)?;
    writeln!(markdown, "- Locus: `{}`", rgv.locus)?;
    writeln!(markdown, "- Viewport: {} px", rgv.viewport_width)?;
    writeln!(markdown, "- Alignment cap: {}\n", rgv.maximum_alignments)?;
    writeln!(markdown, "| Metric | RGV | IGV | RGV speedup |")?;
    writeln!(markdown, "| --- | ---: | ---: | ---: |")?;
    writeln!(
        markdown,
        "| Interaction p50 | {:.3} ms | {:.3} ms | {:.2}× |",
        rgv_interaction.p50,
        igv_interaction.p50,
        igv_interaction.p50 / rgv_interaction.p50
    )?;
    writeln!(
        markdown,
        "| Interaction p95 | {:.3} ms | {:.3} ms | {:.2}× |",
        rgv_interaction.p95,
        igv_interaction.p95,
        igv_interaction.p95 / rgv_interaction.p95
    )?;
    writeln!(
        markdown,
        "| First useful/startup p50 | {:.3} ms | {:.3} ms | {:.2}× |\n",
        rgv.first_useful_ms,
        igv_startup.p50,
        igv_startup.p50 / rgv.first_useful_ms
    )?;
    writeln!(
        markdown,
        "RGV indexed-source open: {:.3} ms.\n",
        rgv.open_ms
    )?;
    writeln!(
        markdown,
        "Outcome: **{}** for first-useful latency and both interaction percentiles.\n",
        if faster { "RGV faster" } else { "gate not met" }
    )?;
    append_optional_summaries(&mut markdown, &rgv, &igv)?;
    writeln!(markdown, "## Commands\n")?;
    for command in &igv.commands {
        writeln!(markdown, "```text\n{command}\n```\n")?;
    }
    writeln!(
        markdown,
        "Raw inputs: `{}` and `{}`.",
        arguments.rgv_report.display(),
        arguments.igv_samples.display()
    )?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, markdown)?;
    println!("wrote {}", arguments.output.display());
    Ok(())
}

fn validate_inputs(arguments: &Arguments, rgv: &RgvReport, igv: &IgvSamples) -> Result<()> {
    anyhow::ensure!(igv.schema_version == 1, "unsupported IGV sample schema");
    anyhow::ensure!(
        rgv.input_sha256.eq_ignore_ascii_case(&igv.fixture_sha256),
        "RGV and IGV fixture hashes differ"
    );
    anyhow::ensure!(rgv.locus == igv.locus, "RGV and IGV loci differ");
    anyhow::ensure!(
        rgv.viewport_width == igv.viewport_width,
        "RGV and IGV viewport widths differ"
    );
    anyhow::ensure!(
        rgv.maximum_alignments == igv.maximum_alignments,
        "RGV and IGV alignment caps differ"
    );
    anyhow::ensure!(!igv.commands.is_empty(), "IGV commands were not recorded");
    if !arguments.allow_development_sample {
        anyhow::ensure!(
            rgv.total_ms.len() >= 10,
            "RGV report has fewer than 10 samples"
        );
        anyhow::ensure!(
            igv.navigation_to_snapshot_ms.len() >= 10,
            "IGV report has fewer than 10 navigation samples"
        );
        anyhow::ensure!(
            igv.startup_to_first_snapshot_ms.len() >= 10,
            "IGV report has fewer than 10 startup samples"
        );
    }
    if arguments.require_complete_telemetry {
        anyhow::ensure!(
            rgv.sampled_rss_mib.len() >= 10,
            "RGV report has fewer than 10 RSS samples"
        );
        require_samples(igv.sampled_rss_mib.as_deref(), "IGV sampled RSS")?;
        require_samples(
            igv.rgv_remote_requests.as_deref(),
            "RGV remote request count",
        )?;
        require_samples(
            igv.rgv_remote_response_bytes.as_deref(),
            "RGV remote response bytes",
        )?;
        require_samples(igv.rgv_remote_wall_ms.as_deref(), "RGV remote wall time")?;
        require_samples(igv.remote_requests.as_deref(), "IGV remote request count")?;
        require_samples(
            igv.remote_response_bytes.as_deref(),
            "IGV remote response bytes",
        )?;
        require_samples(igv.remote_wall_ms.as_deref(), "IGV remote wall time")?;
        require_samples(igv.rgv_cpu_percent.as_deref(), "RGV CPU utilization")?;
        require_samples(igv.cpu_percent.as_deref(), "IGV CPU utilization")?;
    }
    Ok(())
}

fn require_samples<T>(values: Option<&[T]>, label: &str) -> Result<()> {
    anyhow::ensure!(
        values.is_some_and(|values| values.len() >= 10),
        "{label} has fewer than 10 samples"
    );
    Ok(())
}

fn enforce_comparative_telemetry(rgv: &RgvReport, igv: &IgvSamples) -> Result<()> {
    let rgv_rss = summarize(&rgv.sampled_rss_mib)?;
    let igv_rss = summarize(
        igv.sampled_rss_mib
            .as_deref()
            .context("IGV sampled RSS is missing")?,
    )?;
    anyhow::ensure!(
        rgv_rss.p50 < igv_rss.p50 && rgv_rss.p95 < igv_rss.p95,
        "RGV did not beat IGV at both p50 and p95 sampled RSS"
    );

    let rgv_wall = summarize(
        igv.rgv_remote_wall_ms
            .as_deref()
            .context("RGV remote wall samples are missing")?,
    )?;
    let igv_wall = summarize(
        igv.remote_wall_ms
            .as_deref()
            .context("IGV remote wall samples are missing")?,
    )?;
    anyhow::ensure!(
        rgv_wall.p50 < igv_wall.p50 && rgv_wall.p95 < igv_wall.p95,
        "RGV did not beat IGV at both p50 and p95 remote wall time"
    );

    let rgv_bytes = summarize(&as_f64(
        igv.rgv_remote_response_bytes
            .as_deref()
            .context("RGV remote byte samples are missing")?,
    ))?;
    let igv_bytes = summarize(&as_f64(
        igv.remote_response_bytes
            .as_deref()
            .context("IGV remote byte samples are missing")?,
    ))?;
    anyhow::ensure!(
        rgv_bytes.p50 < igv_bytes.p50 && rgv_bytes.p95 < igv_bytes.p95,
        "RGV did not beat IGV at both p50 and p95 remote response bytes"
    );
    Ok(())
}

fn summarize(values: &[f64]) -> Result<Summary> {
    anyhow::ensure!(!values.is_empty(), "sample set is empty");
    anyhow::ensure!(
        values
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0),
        "sample set contains invalid latency"
    );
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Ok(Summary {
        p50: percentile(&sorted, 0.50),
        p95: percentile(&sorted, 0.95),
    })
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn append_optional_summaries(
    markdown: &mut String,
    rgv: &RgvReport,
    igv: &IgvSamples,
) -> Result<()> {
    writeln!(markdown, "## Optional process and remote telemetry\n")?;
    if !rgv.sampled_rss_mib.is_empty() {
        let summary = summarize(&rgv.sampled_rss_mib).context("invalid RGV RSS samples")?;
        writeln!(
            markdown,
            "- RGV sampled RSS (MiB): p50 {:.2}, p95 {:.2}",
            summary.p50, summary.p95
        )?;
    }
    if let Some(values) = &igv.sampled_rss_mib {
        let summary = summarize(values).context("invalid peak RSS samples")?;
        writeln!(
            markdown,
            "- IGV sampled RSS (MiB): p50 {:.2}, p95 {:.2}",
            summary.p50, summary.p95
        )?;
    }
    if let (Some(rgv_cpu), Some(igv_cpu)) =
        (igv.rgv_cpu_percent.as_deref(), igv.cpu_percent.as_deref())
    {
        let rgv_summary = summarize(rgv_cpu).context("invalid RGV CPU samples")?;
        let igv_summary = summarize(igv_cpu).context("invalid IGV CPU samples")?;
        writeln!(
            markdown,
            "- CPU utilization (% of one core): RGV p50 {:.2}/p95 {:.2}; IGV p50 {:.2}/p95 {:.2}",
            rgv_summary.p50, rgv_summary.p95, igv_summary.p50, igv_summary.p95
        )?;
    }
    append_matched_remote_table(markdown, igv)?;
    writeln!(markdown)?;
    Ok(())
}

fn append_matched_remote_table(markdown: &mut String, samples: &IgvSamples) -> Result<()> {
    let pairs = [
        (
            "Requests p50",
            samples.rgv_remote_requests.as_deref().map(as_f64),
            samples.remote_requests.as_deref().map(as_f64),
        ),
        (
            "Response bytes p50",
            samples.rgv_remote_response_bytes.as_deref().map(as_f64),
            samples.remote_response_bytes.as_deref().map(as_f64),
        ),
    ];
    if pairs
        .iter()
        .any(|(_, rgv, igv)| rgv.is_some() && igv.is_some())
        || (samples.rgv_remote_wall_ms.is_some() && samples.remote_wall_ms.is_some())
    {
        writeln!(markdown, "\n| Matched remote metric | RGV | IGV |")?;
        writeln!(markdown, "| --- | ---: | ---: |")?;
        for (label, rgv, igv) in pairs {
            if let (Some(rgv), Some(igv)) = (rgv, igv) {
                writeln!(
                    markdown,
                    "| {label} | {:.0} | {:.0} |",
                    summarize(&rgv)?.p50,
                    summarize(&igv)?.p50
                )?;
            }
        }
        if let (Some(rgv), Some(igv)) = (
            samples.rgv_remote_wall_ms.as_deref(),
            samples.remote_wall_ms.as_deref(),
        ) {
            writeln!(
                markdown,
                "| Wall time p50 | {:.3} ms | {:.3} ms |",
                summarize(rgv)?.p50,
                summarize(igv)?.p50
            )?;
        }
    }
    Ok(())
}

fn as_f64(values: &[u64]) -> Vec<f64> {
    values.iter().map(|value| *value as f64).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles_are_deterministic() {
        let summary = summarize(&[10.0, 1.0, 9.0, 2.0, 8.0, 3.0, 7.0, 4.0, 6.0, 5.0]).unwrap();
        assert!((summary.p50 - 5.0).abs() < f64::EPSILON);
        assert!((summary.p95 - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn invalid_samples_are_rejected() {
        assert!(summarize(&[]).is_err());
        assert!(summarize(&[f64::NAN]).is_err());
    }

    #[test]
    fn complete_comparison_enforces_memory_and_remote_advantage() {
        let rgv = RgvReport {
            input: "fixture.bam".into(),
            locus: "chr1:1-10".to_owned(),
            input_sha256: "abc".to_owned(),
            viewport_width: 1_280,
            maximum_alignments: 50_000,
            open_ms: 1.0,
            first_useful_ms: 10.0,
            sampled_rss_mib: vec![100.0; 10],
            total_ms: vec![1.0; 10],
        };
        let mut igv = IgvSamples {
            schema_version: 1,
            igv_version: "test".to_owned(),
            machine: "test".to_owned(),
            fixture_sha256: "abc".to_owned(),
            locus: "chr1:1-10".to_owned(),
            viewport_width: 1_280,
            maximum_alignments: 50_000,
            startup_to_first_snapshot_ms: vec![20.0; 10],
            navigation_to_snapshot_ms: vec![2.0; 10],
            sampled_rss_mib: Some(vec![200.0; 10]),
            rgv_remote_requests: Some(vec![2; 10]),
            rgv_remote_response_bytes: Some(vec![1_000; 10]),
            rgv_remote_wall_ms: Some(vec![10.0; 10]),
            rgv_cpu_percent: Some(vec![40.0; 10]),
            remote_requests: Some(vec![4; 10]),
            remote_response_bytes: Some(vec![2_000; 10]),
            remote_wall_ms: Some(vec![20.0; 10]),
            cpu_percent: Some(vec![80.0; 10]),
            commands: vec!["igv test".to_owned()],
        };
        enforce_comparative_telemetry(&rgv, &igv).unwrap();
        igv.sampled_rss_mib = Some(vec![50.0; 10]);
        assert!(enforce_comparative_telemetry(&rgv, &igv).is_err());
    }
}
