use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use rgv_automation::{
    AUTOMATION_SCHEMA_VERSION, AutomationBatch, AutomationCommand, AutomationWorkspaceSnapshot,
    INTERACTION_STRESS_SCHEMA_NAME, INTERACTION_STRESS_SCHEMA_VERSION, InteractionRecordingStatus,
    InteractionStressRunManifest, InteractionStressScenario, InteractionStressStep,
    StressFailureClass, StressFailureSignature, StressGeneratorProfile,
    StressGeneratorSpecification, StressRequirements, StressRunEnvironment, StressRunLimits,
    StressRunState,
};
use rgv_interaction_stress::{
    AttachConfiguration, ReplayConfiguration, RunArtifacts, StressRunner, atomic_write_json,
    atomic_write_text, convert_recording, generate_scenario, mutate_scenario, reduce_structurally,
    reduce_with_evaluator,
};

#[derive(Debug, Parser)]
#[command(
    name = "rgv-interaction-stress",
    about = "Record, replay, and generate native macOS interaction stress scenarios for RGV"
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Parse and validate a scenario without attaching to or changing RGV.
    Validate { scenario: PathBuf },
    /// Validate one explicitly controlled RGV instance and write a probe artifact.
    Probe {
        #[arg(long)]
        attach: PathBuf,
        #[arg(long, default_value = "validation/interaction-stress/runs")]
        artifacts: PathBuf,
    },
    /// Stream manual native input received by RGV into a JSONL recording.
    Record {
        #[arg(long)]
        attach: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        fixture: Option<String>,
        #[arg(long)]
        record_text: bool,
        /// Stop automatically after this many seconds; otherwise wait for Ctrl-C.
        #[arg(long)]
        duration_seconds: Option<u64>,
    },
    /// Convert a complete interaction recording into an inspectable scenario.
    Convert {
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        allow_incomplete: bool,
    },
    /// Replay a scenario through guarded native macOS events.
    Replay {
        scenario: PathBuf,
        #[command(flatten)]
        replay: ReplayArgs,
    },
    /// Generate valid actions from live RGV state, then replay or only save them.
    Generate {
        #[command(flatten)]
        replay: ReplayArgs,
        #[arg(long)]
        seed: u64,
        #[arg(long)]
        actions: usize,
        #[arg(long, value_enum, default_value_t = ProfileArg::Aggressive)]
        profile: ProfileArg,
        #[arg(long, default_value_t = 8)]
        maximum_no_wait_actions: usize,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        generate_only: bool,
    },
    /// Mutate timing and paths in an existing recorded or generated scenario.
    Mutate {
        scenario: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        seed: u64,
        #[arg(long, default_value_t = 5)]
        intensity: u8,
    },
    /// Reduce around a typed failure, optionally qualifying candidates with native trials.
    Reduce {
        scenario: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_enum)]
        failure_class: FailureClassArg,
        #[arg(long)]
        step_id: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long, default_value = "failure signature supplied for reduction")]
        message: String,
        /// Run signature-preserving native trials against this attach document.
        #[arg(long)]
        attach: Option<PathBuf>,
        #[arg(
            long,
            default_value = "validation/interaction-stress/runs/reduction-trials"
        )]
        artifacts: PathBuf,
        #[arg(
            long,
            default_value = "tools/interaction-stress/macos-driver/.build/release/rgv-macos-input-driver"
        )]
        driver: PathBuf,
        #[arg(long, default_value_t = 3)]
        attempts: usize,
        #[arg(long, default_value_t = 2)]
        minimum_reproductions: usize,
    },
}

#[derive(Clone, Debug, Args)]
struct ReplayArgs {
    #[arg(long)]
    attach: PathBuf,
    #[arg(long, default_value = "validation/interaction-stress/runs")]
    artifacts: PathBuf,
    #[arg(
        long,
        default_value = "tools/interaction-stress/macos-driver/.build/release/rgv-macos-input-driver"
    )]
    driver: PathBuf,
    /// Resolve and compile all gestures without sending events or mutating RGV.
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    no_diagnostic_trace: bool,
    #[arg(long, default_value_t = 5_000)]
    ui_stall_millis: u64,
    #[arg(long, default_value_t = 30_000)]
    pipeline_stall_millis: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum ProfileArg {
    Smoke,
    RecordingMutation,
    Aggressive,
    Soak,
}

impl From<ProfileArg> for StressGeneratorProfile {
    fn from(value: ProfileArg) -> Self {
        match value {
            ProfileArg::Smoke => Self::Smoke,
            ProfileArg::RecordingMutation => Self::RecordingMutation,
            ProfileArg::Aggressive => Self::Aggressive,
            ProfileArg::Soak => Self::Soak,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum FailureClassArg {
    ProcessExit,
    UiStall,
    PipelineStall,
    ProgressChurn,
    NativeEventMissing,
    NativeEventShapeMismatch,
    ExtraneousNativeInput,
    InteractionLock,
    SemanticMismatch,
    CriticalInvariant,
    TargetGeometryChanged,
    RecordingIncomplete,
    DriverFailure,
    PermissionFailure,
    Cancelled,
}

impl From<FailureClassArg> for StressFailureClass {
    fn from(value: FailureClassArg) -> Self {
        match value {
            FailureClassArg::ProcessExit => Self::ProcessExit,
            FailureClassArg::UiStall => Self::UiStall,
            FailureClassArg::PipelineStall => Self::PipelineStall,
            FailureClassArg::ProgressChurn => Self::ProgressChurn,
            FailureClassArg::NativeEventMissing => Self::NativeEventMissing,
            FailureClassArg::NativeEventShapeMismatch => Self::NativeEventShapeMismatch,
            FailureClassArg::ExtraneousNativeInput => Self::ExtraneousNativeInput,
            FailureClassArg::InteractionLock => Self::InteractionLock,
            FailureClassArg::SemanticMismatch => Self::SemanticMismatch,
            FailureClassArg::CriticalInvariant => Self::CriticalInvariant,
            FailureClassArg::TargetGeometryChanged => Self::TargetGeometryChanged,
            FailureClassArg::RecordingIncomplete => Self::RecordingIncomplete,
            FailureClassArg::DriverFailure => Self::DriverFailure,
            FailureClassArg::PermissionFailure => Self::PermissionFailure,
            FailureClassArg::Cancelled => Self::Cancelled,
        }
    }
}

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CliCommand::Validate { scenario } => {
            let scenario = InteractionStressScenario::read(&scenario)?;
            println!(
                "valid interaction-stress scenario {:?} ({} top-level steps)",
                scenario.name,
                scenario.steps.len()
            );
            Ok(())
        }
        CliCommand::Probe { attach, artifacts } => probe(attach, artifacts).await,
        CliCommand::Record {
            attach,
            output,
            fixture,
            record_text,
            duration_seconds,
        } => record(attach, output, fixture, record_text, duration_seconds).await,
        CliCommand::Convert {
            input,
            output,
            allow_incomplete,
        } => {
            let report = convert_recording(input, allow_incomplete)?;
            atomic_write_text(&output, &report.scenario.to_json()?)?;
            let report_path = output.with_extension("conversion.json");
            atomic_write_json(&report_path, &report)?;
            println!(
                "converted {} gestures ({} fragile, {} unsupported) to {}",
                report.converted_gestures,
                report.fragile_gestures,
                report.unsupported_events,
                output.display()
            );
            Ok(())
        }
        CliCommand::Replay { scenario, replay } => {
            let scenario = InteractionStressScenario::read(scenario)?;
            Box::pin(run_replay(scenario, replay)).await
        }
        CliCommand::Generate {
            replay,
            seed,
            actions,
            profile,
            maximum_no_wait_actions,
            output,
            generate_only,
        } => {
            let specification = StressGeneratorSpecification {
                seed,
                actions,
                profile: profile.into(),
                allowed_actions: Vec::new(),
                maximum_no_wait_actions,
            };
            if generate_only {
                let configuration = AttachConfiguration::read(&replay.attach)?;
                let attached = configuration.attach().await?;
                let workspace = describe_workspace(&attached).await?;
                let scale = attached.status.scale_factor.max(f64::EPSILON);
                let size = [
                    (f64::from(attached.status.surface_width_pixels) / scale).round() as u32,
                    (f64::from(attached.status.surface_height_pixels) / scale).round() as u32,
                ];
                let generated = generate_scenario(
                    &format!("generated {profile:?} seed {seed}"),
                    &workspace,
                    size,
                    &specification,
                )?;
                let output = output.context("--output is required with --generate-only")?;
                atomic_write_text(&output, &generated.scenario.to_json()?)?;
                atomic_write_json(output.with_extension("coverage.json"), &generated)?;
                println!("generated scenario written to {}", output.display());
                Ok(())
            } else {
                let scenario = InteractionStressScenario {
                    schema: INTERACTION_STRESS_SCHEMA_NAME.to_owned(),
                    schema_version: INTERACTION_STRESS_SCHEMA_VERSION,
                    name: format!("generated {profile:?} seed {seed}"),
                    seed: Some(seed),
                    requirements: StressRequirements::default(),
                    steps: vec![InteractionStressStep::Generated { specification }],
                    limits: StressRunLimits {
                        maximum_duration_millis: if profile == ProfileArg::Soak {
                            24 * 60 * 60 * 1_000
                        } else {
                            30 * 60 * 1_000
                        },
                        ..StressRunLimits::default()
                    },
                };
                scenario.validate()?;
                if let Some(output) = output {
                    atomic_write_text(output, &scenario.to_json()?)?;
                }
                Box::pin(run_replay(scenario, replay)).await
            }
        }
        CliCommand::Mutate {
            scenario,
            output,
            seed,
            intensity,
        } => {
            let source = InteractionStressScenario::read(scenario)?;
            let mutated = mutate_scenario(&source, seed, intensity)?;
            atomic_write_text(&output, &mutated.to_json()?)?;
            println!("mutated scenario written to {}", output.display());
            Ok(())
        }
        CliCommand::Reduce {
            scenario,
            output,
            failure_class,
            step_id,
            target,
            owner,
            message,
            attach,
            artifacts,
            driver,
            attempts,
            minimum_reproductions,
        } => {
            let source = InteractionStressScenario::read(scenario)?;
            let signature = StressFailureSignature {
                class: failure_class.into(),
                step_id,
                target,
                owner,
                message,
            };
            let report = if let Some(attach) = attach {
                Box::pin(reduce_natively(
                    &source,
                    signature,
                    attach,
                    artifacts,
                    driver,
                    attempts,
                    minimum_reproductions,
                ))
                .await?
            } else {
                reduce_structurally(&source, signature)?
            };
            atomic_write_text(&output, &report.minimized.to_json()?)?;
            atomic_write_json(output.with_extension("reduction.json"), &report)?;
            println!(
                "reduced {} steps/{} events to {} steps/{} events ({} candidates preserved the signature out of {} attempted)",
                report.original_steps,
                report.original_events,
                report.minimized_steps,
                report.minimized_events,
                report.candidates_preserving_signature,
                report.candidates_attempted,
            );
            Ok(())
        }
    }
}

async fn probe(attach_path: PathBuf, artifacts: PathBuf) -> anyhow::Result<()> {
    let configuration = AttachConfiguration::read(attach_path)?;
    let attached = configuration.attach().await?;
    let workspace = describe_workspace(&attached).await?;
    let health = attached.client.health().await?;
    let parent = resolve_artifact_parent(&attached.control_root, &artifacts)?;
    let run_id = uuid::Uuid::new_v4().to_string();
    let scale = attached.status.scale_factor.max(f64::EPSILON);
    let manifest = InteractionStressRunManifest {
        schema_version: 1,
        run_id,
        state: StressRunState::Running,
        scenario_name: "attach probe".to_owned(),
        scenario_schema: String::new(),
        scenario_schema_version: 0,
        seed: None,
        requirements: StressRequirements::default(),
        limits: StressRunLimits::default(),
        environment: StressRunEnvironment {
            application_version: attached.status.application_version.clone(),
            controller_version: env!("CARGO_PKG_VERSION").to_owned(),
            driver_schema_version: 1,
            automation_schema_version: AUTOMATION_SCHEMA_VERSION,
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            renderer_backend: attached.status.renderer_backend.clone(),
            scale_factor: attached.status.scale_factor,
            initial_surface_size_pixels: [
                attached.status.surface_width_pixels,
                attached.status.surface_height_pixels,
            ],
            initial_window_logical_size: [
                f64::from(attached.status.surface_width_pixels) / scale,
                f64::from(attached.status.surface_height_pixels) / scale,
            ],
            initial_window_global_bounds: None,
        },
        application_instance_id: attached.status.application_instance_id.clone(),
        process_id: attached.status.process_id,
        started_unix_millis: health.sampled_unix_millis,
        finished_unix_millis: None,
        requested_events: 0,
        posted_events: 0,
        received_events: 0,
        applied_events: 0,
        coalesced_events: 0,
        rejected_events: 0,
        emergency_release_events: 0,
        failure: None,
        steps: Vec::new(),
        artifacts: Vec::new(),
    };
    let mut run = RunArtifacts::create(parent, manifest)?;
    run.write_json("control-status.json", "control_status", &attached.status)?;
    run.write_json("workspace.json", "workspace", &workspace)?;
    run.write_json("health.json", "health", &health)?;
    run.finish(StressRunState::Passed, unix_millis())?;
    println!(
        "validated RGV instance; probe artifact: {}",
        run.root().display()
    );
    Ok(())
}

async fn record(
    attach_path: PathBuf,
    output: PathBuf,
    fixture: Option<String>,
    record_text: bool,
    duration_seconds: Option<u64>,
) -> anyhow::Result<()> {
    let configuration = AttachConfiguration::read(attach_path)?;
    let fixture = fixture.or_else(|| configuration.fixture.clone());
    let attached = configuration.attach().await?;
    let output = resolve_output(&attached.control_root, &output)?;
    let start = AutomationBatch {
        schema_version: AUTOMATION_SCHEMA_VERSION,
        commands: vec![AutomationCommand::StartInteractionRecording {
            path: output.display().to_string(),
            fixture,
            record_text,
        }],
    };
    let report = attached.client.run_batch(&start).await?;
    require_success(&report, "start recording")?;
    eprintln!(
        "recording native RGV input to {}; press Ctrl-C to stop",
        output.display()
    );
    if let Some(seconds) = duration_seconds {
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(seconds)) => {}
            result = tokio::signal::ctrl_c() => result.context("could not wait for Ctrl-C")?,
        }
    } else {
        tokio::signal::ctrl_c()
            .await
            .context("could not wait for Ctrl-C")?;
    }
    let stop = AutomationBatch {
        schema_version: AUTOMATION_SCHEMA_VERSION,
        commands: vec![AutomationCommand::StopInteractionRecording],
    };
    let report = attached.client.run_batch(&stop).await?;
    require_success(&report, "stop recording")?;
    let status: InteractionRecordingStatus = serde_json::from_value(
        report
            .results
            .first()
            .context("stop recording returned no result")?
            .data
            .clone(),
    )?;
    if status.state != rgv_automation::InteractionRecordingState::Complete {
        bail!(
            "recording stopped as {:?}: {}",
            status.state,
            status
                .failure
                .unwrap_or_else(|| "incomplete evidence".to_owned())
        );
    }
    println!("recording complete: {}", output.display());
    Ok(())
}

async fn run_replay(scenario: InteractionStressScenario, replay: ReplayArgs) -> anyhow::Result<()> {
    let configuration = AttachConfiguration::read(&replay.attach)?;
    let attached = configuration.attach().await?;
    let cancellation = Arc::new(AtomicBool::new(false));
    let signal_cancellation = Arc::clone(&cancellation);
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.store(true, Ordering::Release);
        }
    });
    let runner = StressRunner::new(
        attached,
        ReplayConfiguration {
            artifact_parent: replay.artifacts,
            driver_path: replay.driver,
            dry_run: replay.dry_run,
            start_diagnostic_trace: !replay.no_diagnostic_trace,
            ui_stall_timeout: Duration::from_millis(replay.ui_stall_millis.max(100)),
            pipeline_stall_timeout: Duration::from_millis(replay.pipeline_stall_millis.max(100)),
            cancellation,
        },
    );
    let result = Box::pin(runner.replay(&scenario)).await;
    signal_task.abort();
    let (path, manifest) = result?;
    println!("run {:?}: {}", manifest.state, path.display());
    if matches!(
        manifest.state,
        StressRunState::Failed | StressRunState::Cancelled
    ) {
        bail!(
            "stress run {:?}: {}",
            manifest.state,
            manifest
                .failure
                .map_or_else(|| "unknown failure".to_owned(), |failure| failure.message)
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn reduce_natively(
    source: &InteractionStressScenario,
    signature: StressFailureSignature,
    attach_path: PathBuf,
    artifact_parent: PathBuf,
    driver_path: PathBuf,
    attempts: usize,
    minimum_reproductions: usize,
) -> anyhow::Result<rgv_interaction_stress::ReductionReport> {
    if attempts == 0 || attempts > 100 {
        bail!("--attempts must be within 1..=100");
    }
    if minimum_reproductions == 0 || minimum_reproductions > attempts {
        bail!("--minimum-reproductions must be within 1..=--attempts");
    }
    if source
        .steps
        .iter()
        .any(|step| matches!(step, InteractionStressStep::Generated { .. }))
    {
        bail!("native reduction requires scenario.expanded.json, not a generated block");
    }
    let attach = AttachConfiguration::read(attach_path)?;
    let cancellation = Arc::new(AtomicBool::new(false));
    let signal_cancellation = Arc::clone(&cancellation);
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.store(true, Ordering::Release);
        }
    });

    if !Box::pin(native_candidate_reproduces(
        source,
        &signature,
        &attach,
        &artifact_parent,
        &driver_path,
        attempts,
        minimum_reproductions,
        &cancellation,
    ))
    .await?
    {
        signal_task.abort();
        bail!(
            "the original scenario did not reproduce the requested signature {minimum_reproductions}/{attempts} times"
        );
    }

    let runtime = tokio::runtime::Handle::current();
    let reduction_attach = attach.clone();
    let reduction_artifacts = artifact_parent.clone();
    let reduction_driver = driver_path.clone();
    let reduction_cancellation = Arc::clone(&cancellation);
    let reduction_signature = signature.clone();
    let report = tokio::task::block_in_place(|| {
        reduce_with_evaluator(source, signature, |candidate| {
            runtime.block_on(Box::pin(native_candidate_reproduces(
                candidate,
                &reduction_signature,
                &reduction_attach,
                &reduction_artifacts,
                &reduction_driver,
                attempts,
                minimum_reproductions,
                &reduction_cancellation,
            )))
        })
    });
    signal_task.abort();
    report
}

#[allow(clippy::too_many_arguments)]
async fn native_candidate_reproduces(
    candidate: &InteractionStressScenario,
    signature: &StressFailureSignature,
    attach: &AttachConfiguration,
    artifact_parent: &std::path::Path,
    driver_path: &std::path::Path,
    attempts: usize,
    minimum_reproductions: usize,
    cancellation: &Arc<AtomicBool>,
) -> anyhow::Result<bool> {
    let mut reproductions = 0_usize;
    for attempt in 0..attempts {
        if cancellation.load(Ordering::Acquire) {
            bail!("native reduction was cancelled");
        }
        let attached = attach.attach().await?;
        let runner = StressRunner::new(
            attached,
            ReplayConfiguration {
                artifact_parent: artifact_parent.to_owned(),
                driver_path: driver_path.to_owned(),
                dry_run: false,
                start_diagnostic_trace: false,
                cancellation: Arc::clone(cancellation),
                ..ReplayConfiguration::default()
            },
        );
        let (_, manifest) = Box::pin(runner.replay(candidate)).await?;
        if manifest
            .failure
            .as_ref()
            .is_some_and(|observed| failure_signature_matches(signature, observed))
        {
            reproductions += 1;
            if reproductions >= minimum_reproductions {
                return Ok(true);
            }
        }
        let remaining = attempts.saturating_sub(attempt + 1);
        if reproductions.saturating_add(remaining) < minimum_reproductions {
            return Ok(false);
        }
    }
    Ok(false)
}

fn failure_signature_matches(
    expected: &StressFailureSignature,
    observed: &StressFailureSignature,
) -> bool {
    expected.class == observed.class
        && expected
            .step_id
            .as_ref()
            .is_none_or(|value| observed.step_id.as_ref() == Some(value))
        && expected
            .target
            .as_ref()
            .is_none_or(|value| observed.target.as_ref() == Some(value))
        && expected
            .owner
            .as_ref()
            .is_none_or(|value| observed.owner.as_ref() == Some(value))
}

async fn describe_workspace(
    attached: &rgv_interaction_stress::AttachedRgv,
) -> anyhow::Result<AutomationWorkspaceSnapshot> {
    let report = attached
        .client
        .run_batch(&AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::DescribeWorkspace],
        })
        .await?;
    require_success(&report, "describe workspace")?;
    serde_json::from_value(
        report
            .results
            .first()
            .context("describe workspace returned no result")?
            .data
            .clone(),
    )
    .context("workspace response was malformed")
}

fn require_success(
    report: &rgv_automation::AutomationRunReport,
    operation: &str,
) -> anyhow::Result<()> {
    if report.succeeded {
        Ok(())
    } else {
        let reason = report.results.iter().find_map(|result| {
            (result.status == rgv_automation::AutomationCommandStatus::Failed)
                .then_some(result.message.as_str())
        });
        bail!(
            "could not {operation}: {}",
            reason.unwrap_or("control batch failed")
        )
    }
}

fn resolve_artifact_parent(
    control_root: &std::path::Path,
    path: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    let candidate = if path.is_relative() {
        control_root.join(path)
    } else {
        path.to_owned()
    };
    std::fs::create_dir_all(&candidate)?;
    let resolved = candidate.canonicalize()?;
    if !resolved.starts_with(control_root) {
        bail!("artifact path is outside the selected RGV control root");
    }
    Ok(resolved)
}

fn resolve_output(
    control_root: &std::path::Path,
    path: &std::path::Path,
) -> anyhow::Result<PathBuf> {
    let candidate = if path.is_relative() {
        control_root.join(path)
    } else {
        path.to_owned()
    };
    let parent = candidate
        .parent()
        .context("recording output has no parent")?;
    std::fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    if !parent.starts_with(control_root) {
        bail!("recording output is outside the selected RGV control root");
    }
    Ok(parent.join(
        candidate
            .file_name()
            .context("recording output has no file name")?,
    ))
}

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_signature_matching_requires_declared_identity_fields() {
        let expected = StressFailureSignature {
            class: StressFailureClass::PipelineStall,
            step_id: Some("gesture-1".to_owned()),
            target: Some("reads".to_owned()),
            owner: None,
            message: "original".to_owned(),
        };
        let mut observed = expected.clone();
        observed.message = "a later trial used different prose".to_owned();
        assert!(failure_signature_matches(&expected, &observed));
        observed.step_id = Some("gesture-2".to_owned());
        assert!(!failure_signature_matches(&expected, &observed));
    }
}
