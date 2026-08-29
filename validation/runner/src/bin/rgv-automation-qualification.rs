use std::{
    collections::{BTreeMap, HashSet},
    fmt::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use clap::Parser;
use rgv_core::{
    AUTOMATION_SCHEMA_VERSION, AutomationBatch, AutomationCommand, AutomationCommandStatus,
    AutomationRunReport,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const QUALIFICATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Parser)]
#[command(about = "Verify a packaged RGV automation run and its generated evidence")]
struct Arguments {
    qualification_manifest: PathBuf,
    automation_report: PathBuf,
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualificationManifest {
    schema_version: u32,
    batch: PathBuf,
    reviewed_artifacts: Vec<ReviewedArtifact>,
    saved_project: SavedProjectExpectation,
    telemetry: TelemetryExpectation,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewedArtifact {
    path: PathBuf,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedProjectExpectation {
    path: PathBuf,
    schema_version: u32,
    active_panel: u64,
    panel_ids: Vec<u64>,
    alignment_tracks: Vec<AlignmentTrackExpectation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlignmentTrackExpectation {
    id: u64,
    group_mode: String,
    group_order: String,
    sort_mode: String,
    sort_order: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TelemetryExpectation {
    operating_system: Option<String>,
    architecture: Option<String>,
    gpu_backend: Option<String>,
    maximum_frame_median_ms: f64,
    maximum_frame_p95_ms: f64,
    maximum_dropped_samples: u64,
    maximum_truncated_frames: u64,
}

#[derive(Debug, Deserialize)]
struct SavedProject {
    schema_version: u32,
    active_panel: u64,
    panels: Vec<SavedPanel>,
    alignment_tracks: Vec<SavedAlignmentTrack>,
}

#[derive(Debug, Deserialize)]
struct SavedPanel {
    id: u64,
}

#[derive(Debug, Deserialize)]
struct SavedAlignmentTrack {
    id: u64,
    filters: SavedAlignmentFilters,
}

#[derive(Debug, Deserialize)]
struct SavedAlignmentFilters {
    #[serde(rename = "alignment_group_mode")]
    group_mode: String,
    #[serde(rename = "alignment_group_order")]
    group_order: String,
    #[serde(rename = "alignment_sort_mode")]
    sort_mode: String,
    #[serde(rename = "alignment_sort_order")]
    sort_order: String,
}

#[derive(Debug, Serialize)]
struct VerificationReport {
    schema_version: u32,
    succeeded: bool,
    qualification_manifest_sha256: String,
    batch_sha256: String,
    automation_report_sha256: String,
    commands_verified: usize,
    artifacts: Vec<VerifiedArtifact>,
    telemetry: VerifiedTelemetry,
}

#[derive(Debug, Serialize)]
struct VerifiedArtifact {
    name: String,
    bytes: u64,
    sha256: String,
    reviewed: bool,
}

#[derive(Debug, Serialize)]
struct VerifiedTelemetry {
    operating_system: String,
    architecture: String,
    gpu_adapter: String,
    gpu_backend: String,
    surface_width_pixels: u32,
    surface_height_pixels: u32,
    scale_factor: f64,
    run_elapsed_ms: f64,
    startup_to_first_frame_ms: f64,
    startup_to_first_settled_ms: f64,
    presented_frames: u64,
    frame_median_ms: f64,
    frame_p95_ms: f64,
    frame_maximum_ms: f64,
    samples_dropped: u64,
    prepared_frames: u64,
    uploaded_frames: u64,
    total_uploaded_bytes: u64,
    peak_uploaded_bytes: usize,
    peak_instances: usize,
    buffer_reallocations: u64,
    truncated_frames: u64,
}

struct EnvironmentEvidence {
    operating_system: String,
    architecture: String,
    gpu_adapter: String,
    gpu_backend: String,
    surface_width_pixels: u32,
    surface_height_pixels: u32,
    scale_factor: f64,
    run_elapsed_ms: f64,
    startup_to_first_frame_ms: f64,
    startup_to_first_settled_ms: f64,
}

struct FrameEvidence {
    presented_frames: u64,
    median_ms: f64,
    p95_ms: f64,
    maximum_ms: f64,
    samples_dropped: u64,
}

fn main() -> Result<()> {
    let arguments = Arguments::parse();
    let verification = verify(&arguments)?;
    let json = format!("{}\n", serde_json::to_string_pretty(&verification)?);
    if let Some(output) = &arguments.output {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(output, json)?;
        println!("wrote {}", output.display());
    } else {
        print!("{json}");
    }
    Ok(())
}

fn verify(arguments: &Arguments) -> Result<VerificationReport> {
    let manifest_bytes = std::fs::read(&arguments.qualification_manifest).with_context(|| {
        format!(
            "could not read qualification manifest {}",
            arguments.qualification_manifest.display()
        )
    })?;
    let manifest: QualificationManifest = serde_json::from_slice(&manifest_bytes)?;
    anyhow::ensure!(
        manifest.schema_version == QUALIFICATION_SCHEMA_VERSION,
        "unsupported qualification manifest schema {}",
        manifest.schema_version
    );
    validate_manifest(&manifest)?;
    let qualification_manifest = std::path::absolute(&arguments.qualification_manifest)?;
    let base = qualification_manifest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let batch_path = base.join(&manifest.batch);
    let batch = AutomationBatch::read(&batch_path)?;
    let report_bytes = std::fs::read(&arguments.automation_report).with_context(|| {
        format!(
            "could not read automation report {}",
            arguments.automation_report.display()
        )
    })?;
    let report: AutomationRunReport = serde_json::from_slice(&report_bytes)?;
    verify_command_results(&batch, &report)?;
    let artifact_paths = verify_command_outputs(&batch, &report)?;
    let reviewed = verify_reviewed_artifacts(base, &manifest.reviewed_artifacts)?;
    verify_saved_project(base, &manifest.saved_project)?;
    let saved_project = base.join(&manifest.saved_project.path).canonicalize()?;
    anyhow::ensure!(
        artifact_paths.contains(&saved_project),
        "the expected saved project was not produced by the automation batch"
    );
    let telemetry = verify_telemetry(&report, &manifest.telemetry)?;
    let artifacts = collect_artifacts(&artifact_paths, &reviewed)?;
    Ok(VerificationReport {
        schema_version: QUALIFICATION_SCHEMA_VERSION,
        succeeded: true,
        qualification_manifest_sha256: sha256_bytes(&manifest_bytes),
        batch_sha256: sha256_file(&batch_path)?,
        automation_report_sha256: sha256_bytes(&report_bytes),
        commands_verified: batch.commands.len(),
        artifacts,
        telemetry,
    })
}

fn validate_manifest(manifest: &QualificationManifest) -> Result<()> {
    anyhow::ensure!(
        !manifest.batch.as_os_str().is_empty(),
        "batch path is empty"
    );
    anyhow::ensure!(
        !manifest.reviewed_artifacts.is_empty(),
        "qualification manifest has no reviewed artifacts"
    );
    let mut reviewed_paths = HashSet::new();
    for artifact in &manifest.reviewed_artifacts {
        anyhow::ensure!(
            !artifact.path.as_os_str().is_empty(),
            "reviewed artifact path is empty"
        );
        anyhow::ensure!(
            reviewed_paths.insert(artifact.path.clone()),
            "duplicate reviewed artifact {}",
            artifact.path.display()
        );
        validate_sha256(&artifact.sha256)?;
    }
    anyhow::ensure!(
        !manifest.saved_project.path.as_os_str().is_empty(),
        "saved project path is empty"
    );
    anyhow::ensure!(
        !manifest.saved_project.panel_ids.is_empty(),
        "saved project expectation has no panels"
    );
    anyhow::ensure!(
        !manifest.saved_project.alignment_tracks.is_empty(),
        "saved project expectation has no alignment tracks"
    );
    let telemetry = &manifest.telemetry;
    for (label, value) in [
        ("maximum frame median", telemetry.maximum_frame_median_ms),
        ("maximum frame p95", telemetry.maximum_frame_p95_ms),
    ] {
        anyhow::ensure!(
            value.is_finite() && value > 0.0,
            "{label} must be finite and positive"
        );
    }
    anyhow::ensure!(
        telemetry.maximum_frame_median_ms <= telemetry.maximum_frame_p95_ms,
        "frame median limit exceeds frame p95 limit"
    );
    Ok(())
}

fn verify_command_results(batch: &AutomationBatch, report: &AutomationRunReport) -> Result<()> {
    anyhow::ensure!(
        report.schema_version == AUTOMATION_SCHEMA_VERSION,
        "unsupported automation report schema {}",
        report.schema_version
    );
    anyhow::ensure!(report.succeeded, "automation report is not successful");
    anyhow::ensure!(
        report.results.len() == batch.commands.len(),
        "automation report has {} results for {} commands",
        report.results.len(),
        batch.commands.len()
    );
    for (index, (command, result)) in batch.commands.iter().zip(&report.results).enumerate() {
        anyhow::ensure!(
            result.command_index == index,
            "result {index} declares command index {}",
            result.command_index
        );
        anyhow::ensure!(
            result.command == command.name(),
            "result {index} names command {} instead of {}",
            result.command,
            command.name()
        );
        anyhow::ensure!(
            result.status == AutomationCommandStatus::Succeeded,
            "command {index} ({}) did not succeed",
            command.name()
        );
        anyhow::ensure!(
            result.error_code.is_none(),
            "successful command {index} retains an error code"
        );
        anyhow::ensure!(
            !result.message.trim().is_empty(),
            "command {index} has no result message"
        );
        anyhow::ensure!(
            result.elapsed_ms.is_finite() && result.elapsed_ms >= 0.0,
            "command {index} has an invalid elapsed time"
        );
    }
    Ok(())
}

fn verify_command_outputs(
    batch: &AutomationBatch,
    report: &AutomationRunReport,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut unique = HashSet::new();
    for (command, result) in batch.commands.iter().zip(&report.results) {
        let Some(expected) = command_output_path(command) else {
            continue;
        };
        let reported = result
            .data
            .get("path")
            .and_then(serde_json::Value::as_str)
            .context("output command result does not contain a path")?;
        anyhow::ensure!(
            Path::new(reported) == expected,
            "{} reported output {} instead of {}",
            command.name(),
            reported,
            expected.display()
        );
        anyhow::ensure!(
            expected.is_file(),
            "{} output is missing: {}",
            command.name(),
            expected.display()
        );
        anyhow::ensure!(
            expected.metadata()?.len() > 0,
            "{} output is empty: {}",
            command.name(),
            expected.display()
        );
        anyhow::ensure!(
            unique.insert(expected.to_path_buf()),
            "duplicate output path {}",
            expected.display()
        );
        paths.push(expected.canonicalize()?);
    }
    anyhow::ensure!(
        !paths.is_empty(),
        "automation batch produced no file evidence"
    );
    Ok(paths)
}

fn command_output_path(command: &AutomationCommand) -> Option<&Path> {
    match command {
        AutomationCommand::Snapshot { path, .. }
        | AutomationCommand::Export { path, .. }
        | AutomationCommand::Save { path } => Some(Path::new(path)),
        _ => None,
    }
}

fn verify_reviewed_artifacts(
    base: &Path,
    expectations: &[ReviewedArtifact],
) -> Result<BTreeMap<PathBuf, String>> {
    let mut reviewed = BTreeMap::new();
    for expectation in expectations {
        let path = base.join(&expectation.path);
        anyhow::ensure!(
            path.is_file(),
            "reviewed artifact is missing: {}",
            path.display()
        );
        let observed = sha256_file(&path)?;
        anyhow::ensure!(
            observed.eq_ignore_ascii_case(&expectation.sha256),
            "reviewed artifact SHA-256 mismatch: {}",
            path.display()
        );
        reviewed.insert(path.canonicalize()?, observed);
    }
    Ok(reviewed)
}

fn verify_saved_project(base: &Path, expectation: &SavedProjectExpectation) -> Result<()> {
    let path = base.join(&expectation.path);
    let project: SavedProject = serde_json::from_slice(
        &std::fs::read(&path)
            .with_context(|| format!("could not read saved project {}", path.display()))?,
    )?;
    anyhow::ensure!(
        project.schema_version == expectation.schema_version,
        "saved project schema differs"
    );
    anyhow::ensure!(
        project.active_panel == expectation.active_panel,
        "saved project active panel differs"
    );
    let panel_ids = project
        .panels
        .iter()
        .map(|panel| panel.id)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        panel_ids == expectation.panel_ids,
        "saved project panel identity/order differs"
    );
    let track_ids = project
        .alignment_tracks
        .iter()
        .map(|track| track.id)
        .collect::<Vec<_>>();
    let expected_track_ids = expectation
        .alignment_tracks
        .iter()
        .map(|track| track.id)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        track_ids == expected_track_ids,
        "saved project alignment-track identity/order differs"
    );
    for expected in &expectation.alignment_tracks {
        let track = project
            .alignment_tracks
            .iter()
            .find(|track| track.id == expected.id)
            .context("expected alignment track is missing from saved project")?;
        anyhow::ensure!(
            track.filters.group_mode == expected.group_mode
                && track.filters.group_order == expected.group_order
                && track.filters.sort_mode == expected.sort_mode
                && track.filters.sort_order == expected.sort_order,
            "saved alignment-track grouping or sorting differs for track {}",
            expected.id
        );
    }
    Ok(())
}

fn verify_telemetry(
    report: &AutomationRunReport,
    expectation: &TelemetryExpectation,
) -> Result<VerifiedTelemetry> {
    let environment = verify_environment(&report.telemetry, expectation)?;
    let frames = verify_frames(&report.telemetry, expectation)?;
    let gpu = &report.telemetry.gpu;
    verify_gpu(gpu, expectation)?;
    Ok(VerifiedTelemetry {
        operating_system: environment.operating_system,
        architecture: environment.architecture,
        gpu_adapter: environment.gpu_adapter,
        gpu_backend: environment.gpu_backend,
        surface_width_pixels: environment.surface_width_pixels,
        surface_height_pixels: environment.surface_height_pixels,
        scale_factor: environment.scale_factor,
        run_elapsed_ms: environment.run_elapsed_ms,
        startup_to_first_frame_ms: environment.startup_to_first_frame_ms,
        startup_to_first_settled_ms: environment.startup_to_first_settled_ms,
        presented_frames: frames.presented_frames,
        frame_median_ms: frames.median_ms,
        frame_p95_ms: frames.p95_ms,
        frame_maximum_ms: frames.maximum_ms,
        samples_dropped: frames.samples_dropped,
        prepared_frames: gpu.prepared_frames,
        uploaded_frames: gpu.uploaded_frames,
        total_uploaded_bytes: gpu.total_uploaded_bytes,
        peak_uploaded_bytes: gpu.peak_uploaded_bytes,
        peak_instances: gpu.peak_instances,
        buffer_reallocations: gpu.buffer_reallocations,
        truncated_frames: gpu.truncated_frames,
    })
}

fn verify_environment(
    telemetry: &rgv_core::AutomationRunTelemetry,
    expectation: &TelemetryExpectation,
) -> Result<EnvironmentEvidence> {
    let environment = &telemetry.environment;
    for (label, value) in [
        (
            "application version",
            environment.application_version.as_str(),
        ),
        ("operating system", environment.operating_system.as_str()),
        ("architecture", environment.architecture.as_str()),
        ("GPU adapter", environment.gpu_adapter.as_str()),
        ("GPU backend", environment.gpu_backend.as_str()),
    ] {
        anyhow::ensure!(
            !value.trim().is_empty(),
            "automation telemetry has no {label}"
        );
    }
    require_optional_match(
        "operating system",
        &environment.operating_system,
        expectation.operating_system.as_deref(),
    )?;
    require_optional_match(
        "architecture",
        &environment.architecture,
        expectation.architecture.as_deref(),
    )?;
    require_optional_match(
        "GPU backend",
        &environment.gpu_backend,
        expectation.gpu_backend.as_deref(),
    )?;
    anyhow::ensure!(
        environment.surface_width_pixels > 0 && environment.surface_height_pixels > 0,
        "automation telemetry has an empty surface"
    );
    require_positive_finite("scale factor", environment.scale_factor)?;
    require_positive_finite("run elapsed time", telemetry.run_elapsed_ms)?;
    let first_frame = telemetry
        .startup_to_first_frame_ms
        .context("startup-to-first-frame telemetry is missing")?;
    let first_settled = telemetry
        .startup_to_first_settled_ms
        .context("startup-to-first-settled telemetry is missing")?;
    require_positive_finite("startup-to-first-frame", first_frame)?;
    require_positive_finite("startup-to-first-settled", first_settled)?;
    anyhow::ensure!(
        first_settled >= first_frame,
        "settled frame precedes the first presented frame"
    );

    Ok(EnvironmentEvidence {
        operating_system: environment.operating_system.clone(),
        architecture: environment.architecture.clone(),
        gpu_adapter: environment.gpu_adapter.clone(),
        gpu_backend: environment.gpu_backend.clone(),
        surface_width_pixels: environment.surface_width_pixels,
        surface_height_pixels: environment.surface_height_pixels,
        scale_factor: environment.scale_factor,
        run_elapsed_ms: telemetry.run_elapsed_ms,
        startup_to_first_frame_ms: first_frame,
        startup_to_first_settled_ms: first_settled,
    })
}

fn verify_frames(
    telemetry: &rgv_core::AutomationRunTelemetry,
    expectation: &TelemetryExpectation,
) -> Result<FrameEvidence> {
    let frames = &telemetry.frames;
    anyhow::ensure!(frames.presented_frames > 0, "no frames were presented");
    anyhow::ensure!(
        !frames.samples_ms.is_empty(),
        "no frame samples were retained"
    );
    anyhow::ensure!(
        frames.presented_frames == u64::try_from(frames.samples_ms.len())? + frames.samples_dropped,
        "presented-frame count does not match retained and dropped samples"
    );
    for sample in &frames.samples_ms {
        require_positive_finite("presented-frame sample", *sample)?;
    }
    let (median, p95, maximum) = summarize_frames(&frames.samples_ms);
    require_summary_match("median", frames.median_ms, median)?;
    require_summary_match("p95", frames.p95_ms, p95)?;
    require_summary_match("maximum", frames.maximum_ms, maximum)?;
    anyhow::ensure!(
        median <= expectation.maximum_frame_median_ms,
        "frame median {median:.3} ms exceeds {:.3} ms",
        expectation.maximum_frame_median_ms
    );
    anyhow::ensure!(
        p95 <= expectation.maximum_frame_p95_ms,
        "frame p95 {p95:.3} ms exceeds {:.3} ms",
        expectation.maximum_frame_p95_ms
    );
    anyhow::ensure!(
        frames.samples_dropped <= expectation.maximum_dropped_samples,
        "dropped frame samples exceed the declared limit"
    );

    Ok(FrameEvidence {
        presented_frames: frames.presented_frames,
        median_ms: median,
        p95_ms: p95,
        maximum_ms: maximum,
        samples_dropped: frames.samples_dropped,
    })
}

fn verify_gpu(
    gpu: &rgv_core::AutomationGpuTelemetry,
    expectation: &TelemetryExpectation,
) -> Result<()> {
    anyhow::ensure!(gpu.prepared_frames > 0, "no GPU frames were prepared");
    anyhow::ensure!(gpu.uploaded_frames > 0, "no GPU frames uploaded data");
    anyhow::ensure!(gpu.total_uploaded_bytes > 0, "no GPU bytes were uploaded");
    anyhow::ensure!(gpu.peak_uploaded_bytes > 0, "peak GPU upload is empty");
    anyhow::ensure!(gpu.peak_instances > 0, "peak GPU instance count is empty");
    anyhow::ensure!(
        gpu.truncated_frames <= expectation.maximum_truncated_frames,
        "truncated GPU frames exceed the declared limit"
    );

    Ok(())
}

fn collect_artifacts(
    paths: &[PathBuf],
    reviewed: &BTreeMap<PathBuf, String>,
) -> Result<Vec<VerifiedArtifact>> {
    let mut artifacts = Vec::with_capacity(paths.len());
    let mut names = HashSet::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("artifact has no portable file name")?
            .to_owned();
        anyhow::ensure!(names.insert(name.clone()), "duplicate artifact name {name}");
        let sha256 = reviewed
            .get(path)
            .cloned()
            .map_or_else(|| sha256_file(path), Ok)?;
        artifacts.push(VerifiedArtifact {
            name,
            bytes: path.metadata()?.len(),
            sha256,
            reviewed: reviewed.contains_key(path),
        });
    }
    anyhow::ensure!(
        artifacts
            .iter()
            .filter(|artifact| artifact.reviewed)
            .count()
            == reviewed.len(),
        "a reviewed artifact was not produced by the automation batch"
    );
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(artifacts)
}

fn require_optional_match(label: &str, observed: &str, expected: Option<&str>) -> Result<()> {
    if let Some(expected) = expected {
        anyhow::ensure!(
            observed == expected,
            "automation telemetry {label} {observed} differs from {expected}"
        );
    }
    Ok(())
}

fn require_positive_finite(label: &str, value: f64) -> Result<()> {
    anyhow::ensure!(
        value.is_finite() && value > 0.0,
        "{label} is not finite and positive"
    );
    Ok(())
}

fn require_summary_match(label: &str, reported: Option<f64>, expected: f64) -> Result<()> {
    let reported = reported.with_context(|| format!("frame {label} is missing"))?;
    anyhow::ensure!(
        reported.to_bits() == expected.to_bits(),
        "frame {label} does not match retained samples"
    );
    Ok(())
}

fn summarize_frames(samples: &[f64]) -> (f64, f64, f64) {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let p95_index = ((sorted.len() as f64 * 0.95).ceil() as usize).clamp(1, sorted.len()) - 1;
    (median, sorted[p95_index], *sorted.last().unwrap())
}

fn validate_sha256(value: &str) -> Result<()> {
    anyhow::ensure!(
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid SHA-256 {value}"
    );
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    Ok(sha256_bytes(&std::fs::read(path)?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}

#[cfg(test)]
mod tests {
    use rgv_core::{
        AutomationCommandResult, AutomationEnvironmentTelemetry, AutomationFrameTelemetry,
        AutomationGpuTelemetry, AutomationRunTelemetry,
    };

    use super::*;

    struct Fixture {
        _directory: tempfile::TempDir,
        manifest: PathBuf,
        report: PathBuf,
        snapshot: PathBuf,
    }

    #[test]
    fn complete_evidence_is_verified_and_summarized() {
        let fixture = fixture();
        let verification = verify(&Arguments {
            qualification_manifest: fixture.manifest,
            automation_report: fixture.report,
            output: None,
        })
        .unwrap();

        assert!(verification.succeeded);
        assert_eq!(verification.commands_verified, 4);
        assert_eq!(verification.artifacts.len(), 2);
        assert!(
            verification
                .artifacts
                .iter()
                .any(|artifact| artifact.reviewed)
        );
        assert!((verification.telemetry.frame_p95_ms - 4.0).abs() < f64::EPSILON);
    }

    #[test]
    fn changed_reviewed_artifact_is_rejected() {
        let fixture = fixture();
        std::fs::write(&fixture.snapshot, "tampered").unwrap();

        let error = verify(&Arguments {
            qualification_manifest: fixture.manifest,
            automation_report: fixture.report,
            output: None,
        })
        .unwrap_err();

        assert!(error.to_string().contains("SHA-256 mismatch"));
    }

    #[test]
    fn mismatched_command_identity_is_rejected() {
        let fixture = fixture();
        let mut report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.report).unwrap()).unwrap();
        report["results"][1]["command"] = serde_json::json!("export");
        std::fs::write(&fixture.report, serde_json::to_vec(&report).unwrap()).unwrap();

        let error = verify(&Arguments {
            qualification_manifest: fixture.manifest,
            automation_report: fixture.report,
            output: None,
        })
        .unwrap_err();

        assert!(error.to_string().contains("names command"));
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let snapshot = root.join("snapshot.svg");
        let project = root.join("project.json");
        let batch = root.join("batch.json");
        let report = root.join("report.json");
        let manifest = root.join("qualification.json");
        std::fs::write(&snapshot, "<svg/>").unwrap();
        write_project(&project);
        write_batch(&batch);
        write_report(&report, &snapshot, &project);
        write_manifest(&manifest, &snapshot);
        Fixture {
            _directory: directory,
            manifest,
            report,
            snapshot,
        }
    }

    fn write_project(project: &Path) {
        std::fs::write(
            project,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 3,
                "active_panel": 2,
                "panels": [{"id": 1}, {"id": 2}],
                "alignment_tracks": [{
                    "id": 1,
                    "filters": {
                        "alignment_group_mode": "strand",
                        "alignment_group_order": "natural",
                        "alignment_sort_mode": "base",
                        "alignment_sort_order": "natural"
                    }
                }]
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn write_batch(batch: &Path) {
        let batch_value = serde_json::json!({
            "schema_version": 1,
            "commands": [
                {"command": "new_workspace"},
                {"command": "snapshot", "path": "snapshot.svg", "format": "svg"},
                {"command": "save", "path": "project.json"},
                {"command": "exit"}
            ]
        });
        std::fs::write(batch, serde_json::to_vec(&batch_value).unwrap()).unwrap();
    }

    fn write_report(report: &Path, snapshot: &Path, project: &Path) {
        let commands = ["new_workspace", "snapshot", "save", "exit"];
        let results = commands
            .iter()
            .enumerate()
            .map(|(index, command)| AutomationCommandResult {
                command_index: index,
                command: (*command).to_owned(),
                status: AutomationCommandStatus::Succeeded,
                error_code: None,
                message: "complete".to_owned(),
                data: match *command {
                    "snapshot" => serde_json::json!({"path": snapshot}),
                    "save" => serde_json::json!({"path": project}),
                    _ => serde_json::Value::Null,
                },
                elapsed_ms: 1.0,
            })
            .collect();
        let frame_samples = vec![1.0, 2.0, 3.0, 4.0];
        let run_report = AutomationRunReport {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            succeeded: true,
            results,
            telemetry: AutomationRunTelemetry {
                run_elapsed_ms: 20.0,
                startup_to_first_frame_ms: Some(10.0),
                startup_to_first_settled_ms: Some(15.0),
                environment: AutomationEnvironmentTelemetry {
                    application_version: "0.1.0".to_owned(),
                    operating_system: "macos".to_owned(),
                    architecture: "aarch64".to_owned(),
                    gpu_adapter: "test GPU".to_owned(),
                    gpu_backend: "Metal".to_owned(),
                    surface_width_pixels: 100,
                    surface_height_pixels: 100,
                    scale_factor: 2.0,
                },
                frames: AutomationFrameTelemetry {
                    presented_frames: 4,
                    samples_ms: frame_samples,
                    samples_dropped: 0,
                    median_ms: Some(3.0),
                    p95_ms: Some(4.0),
                    maximum_ms: Some(4.0),
                },
                gpu: AutomationGpuTelemetry {
                    prepared_frames: 4,
                    uploaded_frames: 2,
                    total_uploaded_bytes: 100,
                    peak_uploaded_bytes: 80,
                    peak_instances: 20,
                    buffer_reallocations: 0,
                    truncated_frames: 0,
                    slab_cache_hits: 0,
                    peak_slab_entries: 0,
                    peak_slab_resident_bytes: 0,
                },
                navigation: Vec::new(),
            },
        };
        std::fs::write(report, run_report.to_json().unwrap()).unwrap();
    }

    fn write_manifest(manifest: &Path, snapshot: &Path) {
        let qualification = serde_json::json!({
            "schema_version": 1,
            "batch": "batch.json",
            "reviewed_artifacts": [{
                "path": "snapshot.svg",
                "sha256": sha256_file(snapshot).unwrap()
            }],
            "saved_project": {
                "path": "project.json",
                "schema_version": 3,
                "active_panel": 2,
                "panel_ids": [1, 2],
                "alignment_tracks": [{
                    "id": 1,
                    "group_mode": "strand",
                    "group_order": "natural",
                    "sort_mode": "base",
                    "sort_order": "natural"
                }]
            },
            "telemetry": {
                "operating_system": "macos",
                "architecture": "aarch64",
                "gpu_backend": "Metal",
                "maximum_frame_median_ms": 16.0,
                "maximum_frame_p95_ms": 33.0,
                "maximum_dropped_samples": 0,
                "maximum_truncated_frames": 0
            }
        });
        std::fs::write(manifest, serde_json::to_vec(&qualification).unwrap()).unwrap();
    }
}
