use std::{
    collections::{BTreeMap, VecDeque},
    fs::OpenOptions,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, bail};
use rgv_core::{
    AUTOMATION_SCHEMA_VERSION, AutomationBatch, AutomationCommand, AutomationRectSnapshot,
    AutomationRunReport, AutomationSnapshotFormat, AutomationSnapshotScope,
    AutomationWorkspaceSnapshot, InteractionStressRunManifest, InteractionStressScenario,
    InteractionStressStep, NativeEventShape, NativeObservationSummary, StressFailureClass,
    StressFailureSignature, StressGeneratorSpecification, StressHealthSnapshot, StressPredicate,
    StressRunEnvironment, StressRunState, StressStepResult, StressTarget,
};

use crate::{
    AttachedRgv, DriverRequest, DriverResponse, OracleBaseline, RunArtifacts, atomic_write_text,
    evaluate_predicate, generate_scenario,
};
use serde::Serialize;

const SHORT_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Default, Serialize)]
struct GenerationCoverage {
    selected_actions: u64,
    failure_free_actions: u64,
    maximum_no_wait_depth: usize,
    action_classes: BTreeMap<String, usize>,
    targets: BTreeMap<String, usize>,
    transition_pairs: BTreeMap<String, usize>,
}

#[derive(Debug, thiserror::Error)]
#[error("stress run was cancelled")]
struct RunCancelled;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct ClassifiedRunFailure {
    class: StressFailureClass,
    message: String,
}

impl ClassifiedRunFailure {
    fn new(class: StressFailureClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }
}

struct HealthWatchdog {
    last_health: StressHealthSnapshot,
    last_ui_progress: Instant,
    last_worker_progress: Instant,
    pending_since: Option<Instant>,
    pending_worker_sequence: u64,
    pending_committed_generation: u64,
}

impl HealthWatchdog {
    fn new(health: StressHealthSnapshot, now: Instant) -> Self {
        let pending = health.desired_generation > health.committed_generation;
        Self {
            pending_since: pending.then_some(now),
            pending_worker_sequence: health.worker_progress_sequence,
            pending_committed_generation: health.committed_generation,
            last_health: health,
            last_ui_progress: now,
            last_worker_progress: now,
        }
    }

    fn observe(
        &mut self,
        health: StressHealthSnapshot,
        now: Instant,
        ui_stall_timeout: Duration,
        pipeline_stall_timeout: Duration,
    ) -> Option<ClassifiedRunFailure> {
        if let Some(code) = &health.terminal_failure_code {
            return Some(ClassifiedRunFailure::new(
                StressFailureClass::CriticalInvariant,
                format!("RGV reported terminal failure {code}"),
            ));
        }
        if health.ui_heartbeat_sequence != self.last_health.ui_heartbeat_sequence {
            self.last_ui_progress = now;
        }
        if health.worker_progress_sequence != self.last_health.worker_progress_sequence
            || health.committed_generation != self.last_health.committed_generation
        {
            self.last_worker_progress = now;
        }

        if health.desired_generation > health.committed_generation {
            let authority_changed = health.desired_generation
                != self.last_health.desired_generation
                || health.committed_generation > self.pending_committed_generation;
            if self.pending_since.is_none() || authority_changed {
                self.pending_since = Some(now);
                self.pending_worker_sequence = health.worker_progress_sequence;
                self.pending_committed_generation = health.committed_generation;
            }
        } else {
            self.pending_since = None;
            self.pending_worker_sequence = health.worker_progress_sequence;
            self.pending_committed_generation = health.committed_generation;
        }

        let failure = if now.duration_since(self.last_ui_progress) >= ui_stall_timeout {
            Some(ClassifiedRunFailure::new(
                StressFailureClass::UiStall,
                "RGV UI heartbeat stopped",
            ))
        } else if health.desired_generation > health.committed_generation
            && now.duration_since(self.last_worker_progress) >= pipeline_stall_timeout
        {
            Some(ClassifiedRunFailure::new(
                StressFailureClass::PipelineStall,
                "RGV desired work made no worker or publication progress",
            ))
        } else if self.pending_since.is_some_and(|pending_since| {
            now.duration_since(pending_since) >= pipeline_stall_timeout.saturating_mul(2)
        }) && health.worker_progress_sequence > self.pending_worker_sequence
            && health.committed_generation <= self.pending_committed_generation
        {
            Some(ClassifiedRunFailure::new(
                StressFailureClass::ProgressChurn,
                "RGV pipeline counters advanced without committing the current authority",
            ))
        } else {
            None
        };
        self.last_health = health;
        failure
    }
}

#[derive(Clone, Debug)]
pub struct ReplayConfiguration {
    pub artifact_parent: PathBuf,
    pub driver_path: PathBuf,
    pub dry_run: bool,
    pub start_diagnostic_trace: bool,
    pub ui_stall_timeout: Duration,
    pub pipeline_stall_timeout: Duration,
    pub cancellation: Arc<AtomicBool>,
}

impl Default for ReplayConfiguration {
    fn default() -> Self {
        Self {
            artifact_parent: PathBuf::from("validation/interaction-stress/runs"),
            driver_path: PathBuf::from(
                "tools/interaction-stress/macos-driver/.build/release/rgv-macos-input-driver",
            ),
            dry_run: false,
            start_diagnostic_trace: true,
            ui_stall_timeout: Duration::from_secs(5),
            pipeline_stall_timeout: Duration::from_secs(30),
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }
}

pub struct StressRunner {
    attached: AttachedRgv,
    configuration: ReplayConfiguration,
    observations: BTreeMap<String, NativeObservationSummary>,
    automation_index: usize,
    driver_index: usize,
    expanded_steps: Vec<InteractionStressStep>,
    baseline: Option<OracleBaseline>,
    current_content_size: [f64; 2],
    current_window_bounds: Option<[f64; 4]>,
    original_pointer_position: Option<[f64; 2]>,
    maximum_held_millis: u64,
    generation_coverage: GenerationCoverage,
    last_generated_action: Option<String>,
}

impl StressRunner {
    pub fn new(attached: AttachedRgv, configuration: ReplayConfiguration) -> Self {
        let scale = attached.status.scale_factor.max(f64::EPSILON);
        let current_content_size = [
            f64::from(attached.status.surface_width_pixels) / scale,
            f64::from(attached.status.surface_height_pixels) / scale,
        ];
        Self {
            attached,
            configuration,
            observations: BTreeMap::new(),
            automation_index: 0,
            driver_index: 0,
            expanded_steps: Vec::new(),
            baseline: None,
            current_content_size,
            current_window_bounds: None,
            original_pointer_position: None,
            maximum_held_millis: 30_000,
            generation_coverage: GenerationCoverage::default(),
            last_generated_action: None,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub async fn replay(
        mut self,
        scenario: &InteractionStressScenario,
    ) -> anyhow::Result<(PathBuf, InteractionStressRunManifest)> {
        scenario.validate()?;
        let artifact_parent = self.resolve_artifact_parent()?;
        let run_id = uuid::Uuid::new_v4().to_string();
        let scale = self.attached.status.scale_factor.max(f64::EPSILON);
        let initial_window_logical_size = [
            f64::from(self.attached.status.surface_width_pixels) / scale,
            f64::from(self.attached.status.surface_height_pixels) / scale,
        ];
        let manifest = InteractionStressRunManifest {
            schema_version: 1,
            run_id: run_id.clone(),
            state: StressRunState::Running,
            scenario_name: scenario.name.clone(),
            scenario_schema: scenario.schema.clone(),
            scenario_schema_version: scenario.schema_version,
            seed: scenario.seed,
            requirements: scenario.requirements.clone(),
            limits: scenario.limits,
            environment: StressRunEnvironment {
                application_version: self.attached.status.application_version.clone(),
                controller_version: env!("CARGO_PKG_VERSION").to_owned(),
                driver_schema_version: crate::driver::DRIVER_SCHEMA_VERSION,
                automation_schema_version: AUTOMATION_SCHEMA_VERSION,
                operating_system: std::env::consts::OS.to_owned(),
                architecture: std::env::consts::ARCH.to_owned(),
                renderer_backend: self.attached.status.renderer_backend.clone(),
                scale_factor: self.attached.status.scale_factor,
                initial_surface_size_pixels: [
                    self.attached.status.surface_width_pixels,
                    self.attached.status.surface_height_pixels,
                ],
                initial_window_logical_size,
                initial_window_global_bounds: None,
            },
            application_instance_id: self.attached.status.application_instance_id.clone(),
            process_id: self.attached.status.process_id,
            started_unix_millis: unix_millis(),
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
        let mut artifacts = RunArtifacts::create(&artifact_parent, manifest)?;
        atomic_write_text(
            artifacts.root().join("scenario.input.json"),
            &scenario.to_json()?,
        )?;
        artifacts.register("scenario", "scenario.input.json", true)?;

        let result = self.run_inner(scenario, &run_id, &mut artifacts).await;
        let mut final_state = match result {
            Ok(()) if self.configuration.dry_run => StressRunState::Incomplete,
            Ok(()) => StressRunState::Passed,
            Err(failure) => {
                artifacts.manifest_mut().failure = Some(failure.clone());
                self.preserve_failure_evidence(&failure, &mut artifacts)
                    .await;
                if failure.class == StressFailureClass::Cancelled {
                    StressRunState::Cancelled
                } else {
                    StressRunState::Failed
                }
            }
        };
        if let Err(error) = self.finish_app_recording(&mut artifacts).await {
            let recording_failure = failure(
                StressFailureClass::RecordingIncomplete,
                None,
                format!("application input evidence was incomplete: {error}"),
            );
            let _ = artifacts.append_json_line(
                "controller-events.jsonl",
                &serde_json::json!({
                    "event": "recording_cleanup_failed",
                    "failure": recording_failure,
                    "at_unix_millis": unix_millis(),
                }),
                true,
            );
            if final_state == StressRunState::Passed {
                artifacts.manifest_mut().failure = Some(recording_failure);
                final_state = StressRunState::Failed;
            }
        }
        if self.configuration.start_diagnostic_trace
            && !self.configuration.dry_run
            && let Ok(report) = self
                .run_control(
                    vec![AutomationCommand::StopDiagnosticTrace],
                    SHORT_CONTROL_TIMEOUT,
                    &mut artifacts,
                )
                .await
        {
            let _ = Self::retain_diagnostic_trace(&report, &mut artifacts);
        }
        if !self.configuration.dry_run && self.original_pointer_position.is_some() {
            if let Err(pointer_failure) = self.restore_original_pointer(&mut artifacts).await {
                let _ = artifacts.append_json_line(
                    "controller-events.jsonl",
                    &serde_json::json!({
                        "event": "pointer_restore_failed",
                        "failure": pointer_failure,
                        "at_unix_millis": unix_millis(),
                    }),
                    true,
                );
                if final_state == StressRunState::Passed {
                    artifacts.manifest_mut().failure = Some(pointer_failure);
                    final_state = StressRunState::Failed;
                }
            }
        }
        let expanded = InteractionStressScenario {
            steps: self.expanded_steps.clone(),
            ..scenario.clone()
        };
        if !expanded.steps.is_empty()
            && expanded.validate().is_ok()
            && let Ok(json) = expanded.to_json()
        {
            let _ = atomic_write_text(artifacts.root().join("scenario.expanded.json"), &json);
            let _ = artifacts.register("expanded_scenario", "scenario.expanded.json", true);
        }
        let expansion_partial = artifacts.root().join("scenario-expansion.partial.jsonl");
        if expansion_partial.is_file() {
            let expansion_complete = artifacts.root().join("scenario-expansion.jsonl");
            if std::fs::rename(&expansion_partial, &expansion_complete).is_ok() {
                let _ = artifacts.register(
                    "scenario_expansion_journal",
                    "scenario-expansion.jsonl",
                    true,
                );
            } else {
                let _ = artifacts.register(
                    "scenario_expansion_journal",
                    "scenario-expansion.partial.jsonl",
                    false,
                );
            }
        }
        for (relative, kind) in [
            ("health.jsonl", "health_stream"),
            ("driver-events.jsonl", "driver_event_stream"),
            ("controller-events.jsonl", "controller_event_stream"),
        ] {
            if artifacts.root().join(relative).is_file() {
                let _ = artifacts.register(kind, relative, true);
            }
        }
        if self.generation_coverage.selected_actions > 0 {
            let _ = artifacts.write_json(
                "generation-coverage.json",
                "generation_coverage",
                &self.generation_coverage,
            );
        }
        artifacts.finish(final_state, unix_millis())?;
        Ok((artifacts.root().to_owned(), artifacts.manifest().clone()))
    }

    #[allow(clippy::too_many_lines)]
    async fn run_inner(
        &mut self,
        scenario: &InteractionStressScenario,
        run_id: &str,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        self.maximum_held_millis = scenario.limits.maximum_held_millis;
        let initial_workspace = self
            .describe_workspace(artifacts)
            .await
            .map_err(infrastructure_failure)?;
        let initial_health = self
            .attached
            .client
            .health()
            .await
            .map_err(|error| infrastructure_failure(error.into()))?;
        validate_identity(
            &self.attached.status.application_instance_id,
            self.attached.status.process_id,
            &initial_workspace,
            &initial_health,
        )?;
        self.validate_requirements(scenario, &initial_workspace)?;
        self.baseline = Some(OracleBaseline::new(
            initial_workspace.clone(),
            initial_health.clone(),
        ));
        artifacts
            .append_json_line("health.jsonl", &initial_health, false)
            .map_err(infrastructure_failure)?;

        if self.configuration.dry_run {
            self.compile_only(scenario, &initial_workspace, run_id, artifacts)?;
            return Ok(());
        }
        if !cfg!(target_os = "macos") {
            return Err(failure(
                StressFailureClass::DriverFailure,
                None,
                "native replay is supported only on macOS",
            ));
        }
        if !self.configuration.driver_path.is_file() {
            return Err(failure(
                StressFailureClass::DriverFailure,
                None,
                format!(
                    "macOS driver was not found at {}",
                    self.configuration.driver_path.display()
                ),
            ));
        }
        let preflight = DriverRequest::preflight(
            self.attached.status.process_id,
            self.attached.status.application_instance_id.clone(),
            self.attached.expected_process_name.clone(),
            self.current_content_size,
        );
        let response = self.invoke_driver(&preflight, artifacts).await?;
        if response.permission != crate::driver::DriverPermission::Granted {
            return Err(failure(
                StressFailureClass::PermissionFailure,
                None,
                response
                    .failure
                    .unwrap_or_else(|| "macOS input permission was not granted".to_owned()),
            ));
        }
        if !response.succeeded {
            return Err(failure(
                StressFailureClass::DriverFailure,
                None,
                response
                    .failure
                    .unwrap_or_else(|| "macOS input-driver preflight failed".to_owned()),
            ));
        }
        self.original_pointer_position = response.pointer_position;
        if self.original_pointer_position.is_none() {
            return Err(failure(
                StressFailureClass::DriverFailure,
                None,
                "macOS input-driver preflight did not capture the original pointer position",
            ));
        }
        if let Some(window) = response.window {
            self.current_content_size = window.content_size;
            self.current_window_bounds = Some(window.global_bounds);
            artifacts
                .manifest_mut()
                .environment
                .initial_window_global_bounds = Some(window.global_bounds);
        }
        if self.configuration.start_diagnostic_trace {
            self.run_control(
                vec![AutomationCommand::StartDiagnosticTrace],
                SHORT_CONTROL_TIMEOUT,
                artifacts,
            )
            .await
            .map_err(infrastructure_failure)?;
        }
        let recording_path = artifacts.root().join("app-input-events.jsonl");
        self.run_control(
            vec![AutomationCommand::StartInteractionRecording {
                path: recording_path.display().to_string(),
                fixture: scenario.requirements.fixture.clone(),
                record_text: false,
            }],
            SHORT_CONTROL_TIMEOUT,
            artifacts,
        )
        .await
        .map_err(infrastructure_failure)?;

        let mut queue = VecDeque::from(flatten_repeats(&scenario.steps));
        let run_started = Instant::now();
        let mut step_index = 0;
        while let Some(step) = queue.pop_front() {
            if self.configuration.cancellation.load(Ordering::Acquire) {
                return Err(failure(
                    StressFailureClass::Cancelled,
                    None,
                    "stress run was cancelled",
                ));
            }
            if run_started.elapsed().as_millis()
                > u128::from(scenario.limits.maximum_duration_millis)
            {
                return Err(failure(
                    StressFailureClass::Cancelled,
                    None,
                    "scenario exceeded its overall duration limit",
                ));
            }
            if let InteractionStressStep::Generated { specification } = step {
                self.execute_generated(specification, run_id, &mut step_index, artifacts)
                    .await?;
                continue;
            }
            Self::persist_expanded_step(&step, artifacts)?;
            self.execute_step(step_index, &step, run_id, artifacts)
                .await?;
            self.expanded_steps.push(step);
            step_index += 1;
        }
        Ok(())
    }

    fn compile_only(
        &mut self,
        scenario: &InteractionStressScenario,
        workspace: &AutomationWorkspaceSnapshot,
        run_id: &str,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        let mut steps = flatten_repeats(&scenario.steps);
        let mut expanded = Vec::new();
        for step in steps.drain(..) {
            match step {
                InteractionStressStep::Generated { specification } => {
                    let generated = generate_scenario(
                        "dry-run generated block",
                        workspace,
                        [
                            self.current_content_size[0].round() as u32,
                            self.current_content_size[1].round() as u32,
                        ],
                        &specification,
                    )
                    .map_err(infrastructure_failure)?;
                    expanded.extend(
                        generated
                            .scenario
                            .steps
                            .into_iter()
                            .filter(|step| matches!(step, InteractionStressStep::Gesture { .. })),
                    );
                }
                other => expanded.push(other),
            }
        }
        for step in expanded {
            Self::persist_expanded_step(&step, artifacts)?;
            if let InteractionStressStep::Gesture {
                id,
                target,
                timing,
                events,
                ..
            } = &step
            {
                let rect = resolve_target_rect(target, workspace, self.current_content_size)?;
                let request = DriverRequest::compile_gesture(
                    self.attached.status.process_id,
                    self.attached.status.application_instance_id.clone(),
                    run_id.to_owned(),
                    id.clone(),
                    self.attached.expected_process_name.clone(),
                    self.current_content_size,
                    self.current_window_bounds,
                    rect,
                    events,
                    *timing,
                    scenario.limits.maximum_held_millis,
                )
                .map_err(infrastructure_failure)?;
                self.driver_index += 1;
                artifacts
                    .write_json(
                        format!("driver/request-{:05}.json", self.driver_index),
                        "compiled_driver_request",
                        &request,
                    )
                    .map_err(infrastructure_failure)?;
            }
            self.expanded_steps.push(step);
        }
        Ok(())
    }

    async fn execute_generated(
        &mut self,
        specification: StressGeneratorSpecification,
        run_id: &str,
        step_index: &mut usize,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        let mut no_wait = 0;
        for action_index in 0..specification.actions {
            let workspace = self
                .describe_workspace(artifacts)
                .await
                .map_err(infrastructure_failure)?;
            let mut one = specification.clone();
            one.actions = 1;
            one.seed = mix_seed(specification.seed, action_index as u64);
            let generated = generate_scenario(
                "online generated action",
                &workspace,
                [
                    self.current_content_size[0].round() as u32,
                    self.current_content_size[1].round() as u32,
                ],
                &one,
            )
            .map_err(infrastructure_failure)?;
            let action = generated
                .action_coverage
                .keys()
                .next()
                .cloned()
                .unwrap_or_else(|| "unknown".to_owned());
            self.generation_coverage.selected_actions =
                self.generation_coverage.selected_actions.saturating_add(1);
            for (label, count) in &generated.action_coverage {
                *self
                    .generation_coverage
                    .action_classes
                    .entry(label.clone())
                    .or_default() += count;
            }
            for (label, count) in &generated.target_coverage {
                *self
                    .generation_coverage
                    .targets
                    .entry(label.clone())
                    .or_default() += count;
            }
            if let Some(previous) = self.last_generated_action.replace(action.clone()) {
                *self
                    .generation_coverage
                    .transition_pairs
                    .entry(format!("{previous}->{action}"))
                    .or_default() += 1;
            }
            let Some(mut gesture) = generated
                .scenario
                .steps
                .into_iter()
                .find(|step| matches!(step, InteractionStressStep::Gesture { .. }))
            else {
                return Err(infrastructure_failure(anyhow::anyhow!(
                    "online generator returned no gesture"
                )));
            };
            if let InteractionStressStep::Gesture { id, .. } = &mut gesture {
                *id = format!("generated-{:05}-{action_index:05}", *step_index);
            }
            Self::persist_expanded_step(&gesture, artifacts)?;
            self.execute_step(*step_index, &gesture, run_id, artifacts)
                .await?;
            self.generation_coverage.failure_free_actions = self
                .generation_coverage
                .failure_free_actions
                .saturating_add(1);
            self.expanded_steps.push(gesture);
            *step_index += 1;
            no_wait += 1;
            self.generation_coverage.maximum_no_wait_depth =
                self.generation_coverage.maximum_no_wait_depth.max(no_wait);
            if no_wait >= specification.maximum_no_wait_actions {
                let barrier = InteractionStressStep::Barrier {
                    predicate: StressPredicate::WorkspaceSettled,
                    timeout_millis: 120_000,
                };
                Self::persist_expanded_step(&barrier, artifacts)?;
                self.execute_step(*step_index, &barrier, run_id, artifacts)
                    .await?;
                self.expanded_steps.push(barrier);
                *step_index += 1;
                no_wait = 0;
            }
        }
        Ok(())
    }

    async fn execute_step(
        &mut self,
        index: usize,
        step: &InteractionStressStep,
        run_id: &str,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        let started = Instant::now();
        let result = match step {
            InteractionStressStep::Control { commands } => self
                .run_control(commands.clone(), Duration::from_secs(610), artifacts)
                .await
                .map(|_| ())
                .map_err(infrastructure_failure),
            InteractionStressStep::Gesture {
                id,
                target,
                timing,
                events,
                ..
            } => {
                self.execute_gesture(id, target, *timing, events, run_id, artifacts)
                    .await
            }
            InteractionStressStep::Barrier {
                predicate,
                timeout_millis,
            } => {
                self.wait_for_predicate(
                    predicate,
                    Duration::from_millis(*timeout_millis),
                    artifacts,
                )
                .await
            }
            InteractionStressStep::Assert { predicate } => {
                self.assert_predicate(predicate, artifacts).await
            }
            InteractionStressStep::Checkpoint {
                label,
                capture_native_window,
            } => {
                self.checkpoint(index, label, *capture_native_window, artifacts)
                    .await
            }
            InteractionStressStep::Generated { .. } | InteractionStressStep::Repeat { .. } => {
                unreachable!("orchestration steps are expanded before execution")
            }
        };
        let failure = result.as_ref().err().cloned();
        artifacts.manifest_mut().steps.push(StressStepResult {
            index,
            kind: step_kind(step).to_owned(),
            succeeded: result.is_ok(),
            elapsed_millis: started.elapsed().as_secs_f64() * 1_000.0,
            failure,
        });
        let _ = artifacts.persist_manifest();
        result
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_gesture(
        &mut self,
        step_id: &str,
        target: &StressTarget,
        timing: rgv_core::StressTiming,
        events: &[rgv_core::StressNativeEvent],
        run_id: &str,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        let before_workspace = self
            .describe_workspace(artifacts)
            .await
            .map_err(infrastructure_failure)?;
        let before_health = self
            .attached
            .client
            .health()
            .await
            .map_err(|error| infrastructure_failure(error.into()))?;
        self.baseline = Some(OracleBaseline::new(
            before_workspace.clone(),
            before_health.clone(),
        ));
        let layout_revision = before_workspace
            .workspace_layout
            .as_ref()
            .map(|layout| layout.revision);
        let target_rect =
            resolve_target_rect(target, &before_workspace, self.current_content_size)?;
        self.run_control(
            vec![AutomationCommand::BeginNativeObservation {
                run_id: run_id.to_owned(),
                step_id: step_id.to_owned(),
                expected_target: Some(target.clone()),
            }],
            SHORT_CONTROL_TIMEOUT,
            artifacts,
        )
        .await
        .map_err(infrastructure_failure)?;
        let request = match DriverRequest::compile_gesture(
            self.attached.status.process_id,
            self.attached.status.application_instance_id.clone(),
            run_id.to_owned(),
            step_id.to_owned(),
            self.attached.expected_process_name.clone(),
            self.current_content_size,
            self.current_window_bounds,
            target_rect,
            events,
            timing,
            self.maximum_held_millis,
        ) {
            Ok(request) => request,
            Err(error) => {
                let _ = self
                    .run_control(
                        vec![AutomationCommand::EndNativeObservation],
                        SHORT_CONTROL_TIMEOUT,
                        artifacts,
                    )
                    .await;
                return Err(infrastructure_failure(error));
            }
        };
        artifacts.manifest_mut().requested_events = artifacts
            .manifest()
            .requested_events
            .saturating_add(request.events.len() as u64);
        let response = match self.invoke_driver(&request, artifacts).await {
            Ok(response) => response,
            Err(failure) => {
                let _ = self
                    .run_control(
                        vec![AutomationCommand::EndNativeObservation],
                        SHORT_CONTROL_TIMEOUT,
                        artifacts,
                    )
                    .await;
                return Err(failure);
            }
        };
        let posted = response
            .acknowledgements
            .iter()
            .filter(|acknowledgement| acknowledgement.succeeded)
            .count() as u64;
        artifacts.manifest_mut().posted_events =
            artifacts.manifest().posted_events.saturating_add(posted);
        if !response.succeeded || posted != request.events.len() as u64 {
            let _ = self
                .run_control(
                    vec![AutomationCommand::EndNativeObservation],
                    SHORT_CONTROL_TIMEOUT,
                    artifacts,
                )
                .await;
            return Err(failure(
                StressFailureClass::DriverFailure,
                Some(step_id.to_owned()),
                response
                    .failure
                    .unwrap_or_else(|| "driver did not acknowledge every event".to_owned()),
            ));
        }
        if let Some(window) = &response.window {
            self.current_content_size = window.content_size;
            self.current_window_bounds = Some(window.global_bounds);
        }
        let report = self
            .run_control(
                vec![AutomationCommand::EndNativeObservation],
                SHORT_CONTROL_TIMEOUT,
                artifacts,
            )
            .await
            .map_err(infrastructure_failure)?;
        let summary: NativeObservationSummary = report
            .results
            .first()
            .ok_or_else(|| {
                failure(
                    StressFailureClass::NativeEventMissing,
                    Some(step_id.to_owned()),
                    "RGV returned no native observation result",
                )
            })?
            .data
            .clone()
            .pipe(serde_json::from_value)
            .map_err(|error| infrastructure_failure(error.into()))?;
        if summary.run_id != run_id || summary.step_id != step_id {
            return Err(failure(
                StressFailureClass::NativeEventMissing,
                Some(step_id.to_owned()),
                "RGV returned native-input evidence for a different run or step",
            ));
        }
        artifacts.manifest_mut().received_events = artifacts
            .manifest()
            .received_events
            .saturating_add(summary.received_events);
        artifacts.manifest_mut().applied_events = artifacts
            .manifest()
            .applied_events
            .saturating_add(summary.applied_events);
        validate_native_shape(step_id, events, &summary)?;
        self.observations.insert(step_id.to_owned(), summary);

        let after = self
            .describe_workspace(artifacts)
            .await
            .map_err(infrastructure_failure)?;
        let resized = events
            .iter()
            .any(|event| matches!(event, rgv_core::StressNativeEvent::ResizeWindow { .. }));
        let after_revision = after
            .workspace_layout
            .as_ref()
            .map(|layout| layout.revision);
        if !resized && layout_revision.is_some() && layout_revision != after_revision {
            return Err(failure(
                StressFailureClass::TargetGeometryChanged,
                Some(step_id.to_owned()),
                "workspace geometry changed during native gesture delivery",
            ));
        }
        Ok(())
    }

    async fn assert_predicate(
        &mut self,
        predicate: &StressPredicate,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        let workspace = self
            .describe_workspace(artifacts)
            .await
            .map_err(infrastructure_failure)?;
        let health = self
            .attached
            .client
            .health()
            .await
            .map_err(|error| infrastructure_failure(error.into()))?;
        artifacts
            .append_json_line("health.jsonl", &health, false)
            .map_err(infrastructure_failure)?;
        let baseline = self.baseline.as_ref().ok_or_else(|| {
            infrastructure_failure(anyhow::anyhow!("oracle baseline is unavailable"))
        })?;
        evaluate_predicate(predicate, baseline, &workspace, &health, &self.observations)
    }

    async fn wait_for_predicate(
        &mut self,
        predicate: &StressPredicate,
        timeout: Duration,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        let initial_health = self
            .attached
            .client
            .health()
            .await
            .map_err(|error| infrastructure_failure(error.into()))?;
        let deadline = Instant::now() + timeout;
        let mut last_failure = None;
        while Instant::now() < deadline {
            if self.configuration.cancellation.load(Ordering::Acquire) {
                return Err(failure(
                    StressFailureClass::Cancelled,
                    None,
                    "stress run was cancelled while waiting for a predicate",
                ));
            }
            match self.assert_predicate(predicate, artifacts).await {
                Ok(()) => return Ok(()),
                Err(failure) => last_failure = Some(failure),
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
        if let Ok(final_health) = self.attached.client.health().await
            && final_health.desired_generation > final_health.committed_generation
            && final_health.worker_progress_sequence > initial_health.worker_progress_sequence
            && final_health.committed_generation <= initial_health.committed_generation
        {
            return Err(failure(
                StressFailureClass::ProgressChurn,
                None,
                "pipeline counters advanced without committing the current desired generation",
            ));
        }
        Err(last_failure.unwrap_or_else(|| {
            failure(
                StressFailureClass::PipelineStall,
                None,
                "predicate did not pass before its outer deadline",
            )
        }))
    }

    async fn checkpoint(
        &mut self,
        index: usize,
        label: &str,
        capture_native_window: bool,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        self.run_control(
            vec![AutomationCommand::CheckpointInteractionRecording {
                label: label.to_owned(),
            }],
            SHORT_CONTROL_TIMEOUT,
            artifacts,
        )
        .await
        .map_err(infrastructure_failure)?;
        let workspace = self
            .describe_workspace(artifacts)
            .await
            .map_err(infrastructure_failure)?;
        let health = self
            .attached
            .client
            .health()
            .await
            .map_err(|error| infrastructure_failure(error.into()))?;
        artifacts
            .write_json(
                format!("checkpoint-{index:05}.json"),
                "checkpoint",
                &serde_json::json!({"label": label, "workspace": workspace, "health": health}),
            )
            .map_err(infrastructure_failure)?;
        if capture_native_window {
            let relative = format!("diagnostics/checkpoint-{index:05}.png");
            let output = artifacts.root().join(&relative);
            self.run_control(
                vec![AutomationCommand::Snapshot {
                    path: output.display().to_string(),
                    scope: AutomationSnapshotScope::NativeWindow,
                    format: AutomationSnapshotFormat::Png,
                }],
                Duration::from_secs(60),
                artifacts,
            )
            .await
            .map_err(infrastructure_failure)?;
            artifacts
                .register("diagnostic_capture", relative, true)
                .map_err(infrastructure_failure)?;
        }
        Ok(())
    }

    async fn describe_workspace(
        &mut self,
        artifacts: &mut RunArtifacts,
    ) -> anyhow::Result<AutomationWorkspaceSnapshot> {
        let report = self
            .run_control(
                vec![AutomationCommand::DescribeWorkspace],
                SHORT_CONTROL_TIMEOUT,
                artifacts,
            )
            .await?;
        let result = report
            .results
            .first()
            .context("describe_workspace returned no command result")?;
        serde_json::from_value(result.data.clone()).context("workspace snapshot was malformed")
    }

    async fn run_control(
        &mut self,
        commands: Vec<AutomationCommand>,
        timeout: Duration,
        artifacts: &mut RunArtifacts,
    ) -> anyhow::Result<AutomationRunReport> {
        self.automation_index += 1;
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands,
        };
        batch.validate()?;
        let batch_name = format!("automation/batch-{:05}.json", self.automation_index);
        atomic_write_text(artifacts.root().join(&batch_name), &batch.to_json()?)?;
        artifacts.register("automation_batch", &batch_name, true)?;
        let request = self.attached.client.run_batch_with_timeout(&batch, timeout);
        tokio::pin!(request);
        let mut watchdog = None;
        let report = loop {
            tokio::select! {
                result = &mut request => break result?,
                () = tokio::time::sleep(HEALTH_POLL_INTERVAL) => {
                    if self.configuration.cancellation.load(Ordering::Acquire) {
                        return Err(RunCancelled.into());
                    }
                    let health = self.attached.client.health().await.map_err(|error| {
                        ClassifiedRunFailure::new(
                            StressFailureClass::ProcessExit,
                            format!("RGV health became unavailable during control wait: {error}"),
                        )
                    })?;
                    ensure_health_identity(&self.attached, &health)?;
                    artifacts.append_json_line("health.jsonl", &health, false)?;
                    let now = Instant::now();
                    let active = watchdog.get_or_insert_with(|| HealthWatchdog::new(health.clone(), now));
                    if let Some(failure) = active.observe(
                        health,
                        now,
                        self.configuration.ui_stall_timeout,
                        self.configuration.pipeline_stall_timeout,
                    ) {
                        return Err(failure.into());
                    }
                }
            }
        };
        let report_name = format!("automation/report-{:05}.json", self.automation_index);
        artifacts.write_json(&report_name, "automation_report", &report)?;
        if !report.succeeded {
            let detail = report
                .results
                .iter()
                .find(|result| result.status == rgv_core::AutomationCommandStatus::Failed)
                .map_or_else(
                    || "RGV control batch failed".to_owned(),
                    |result| format!("{}: {}", result.command, result.message),
                );
            bail!(detail);
        }
        Ok(report)
    }

    #[allow(clippy::too_many_lines)]
    async fn invoke_driver(
        &mut self,
        request: &DriverRequest,
        artifacts: &mut RunArtifacts,
    ) -> Result<DriverResponse, StressFailureSignature> {
        self.driver_index += 1;
        let request_relative = format!("driver/request-{:05}.json", self.driver_index);
        let response_relative = format!("driver/response-{:05}.json", self.driver_index);
        let stderr_relative = format!("driver/stderr-{:05}.txt", self.driver_index);
        let request_path = artifacts
            .write_json(&request_relative, "driver_request", request)
            .map_err(infrastructure_failure)?;
        let response_path = artifacts.root().join(&response_relative);
        let stderr_path = artifacts.root().join(&stderr_relative);
        if let Some(parent) = stderr_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| infrastructure_failure(error.into()))?;
        }
        let stderr = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&stderr_path)
            .map_err(|error| infrastructure_failure(error.into()))?;
        let mut child = Command::new(&self.configuration.driver_path)
            .arg("--request")
            .arg(&request_path)
            .arg("--response")
            .arg(&response_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(|error| {
                failure(
                    StressFailureClass::DriverFailure,
                    None,
                    format!("could not start macOS input driver: {error}"),
                )
            })?;
        let deadline = Instant::now()
            + Duration::from_millis(request.maximum_duration_millis.saturating_add(5_000));
        let initial_health = self
            .attached
            .client
            .health()
            .await
            .map_err(|error| infrastructure_failure(error.into()))?;
        if let Err(error) = ensure_health_identity(&self.attached, &initial_health) {
            self.stop_driver_and_release(&mut child, request, artifacts);
            return Err(failure(error.class, None, error.message));
        }
        let mut watchdog = HealthWatchdog::new(initial_health, Instant::now());
        let exit_status = loop {
            if self.configuration.cancellation.load(Ordering::Acquire) {
                self.stop_driver_and_release(&mut child, request, artifacts);
                return Err(failure(
                    StressFailureClass::Cancelled,
                    None,
                    "stress run was cancelled during native delivery",
                ));
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    artifacts
                        .register("driver_stderr", &stderr_relative, true)
                        .map_err(infrastructure_failure)?;
                    break status;
                }
                Ok(None) => {}
                Err(error) => {
                    self.stop_driver_and_release(&mut child, request, artifacts);
                    return Err(failure(
                        StressFailureClass::DriverFailure,
                        None,
                        format!("could not inspect macOS input driver: {error}"),
                    ));
                }
            }
            let health = match self.attached.client.health().await {
                Ok(health) => health,
                Err(error) => {
                    self.stop_driver_and_release(&mut child, request, artifacts);
                    return Err(failure(
                        StressFailureClass::ProcessExit,
                        None,
                        format!("RGV health became unavailable during delivery: {error}"),
                    ));
                }
            };
            if let Err(error) = ensure_health_identity(&self.attached, &health) {
                self.stop_driver_and_release(&mut child, request, artifacts);
                return Err(failure(error.class, None, error.message));
            }
            artifacts
                .append_json_line("health.jsonl", &health, false)
                .map_err(infrastructure_failure)?;
            if let Some(error) = watchdog.observe(
                health,
                Instant::now(),
                self.configuration.ui_stall_timeout,
                self.configuration.pipeline_stall_timeout,
            ) {
                self.stop_driver_and_release(&mut child, request, artifacts);
                return Err(failure(error.class, None, error.message));
            }
            if Instant::now() >= deadline {
                self.stop_driver_and_release(&mut child, request, artifacts);
                return Err(failure(
                    StressFailureClass::DriverFailure,
                    None,
                    "macOS input driver exceeded its outer deadline",
                ));
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        };
        let response = match DriverResponse::read(&response_path, &request.request_id) {
            Ok(response) => response,
            Err(error) => {
                if response_path.is_file() {
                    let _ = artifacts.register("driver_response", &response_relative, false);
                }
                if !exit_status.success() {
                    self.emergency_release_after_interruption(request, artifacts);
                }
                return Err(failure(
                    StressFailureClass::DriverFailure,
                    None,
                    format!(
                        "macOS input driver exited with {exit_status} without a valid response: {error}"
                    ),
                ));
            }
        };
        artifacts
            .register("driver_response", &response_relative, true)
            .map_err(infrastructure_failure)?;
        artifacts
            .append_json_line("driver-events.jsonl", &response, false)
            .map_err(infrastructure_failure)?;
        artifacts.manifest_mut().emergency_release_events = artifacts
            .manifest()
            .emergency_release_events
            .saturating_add(response.emergency_releases);
        Ok(response)
    }

    async fn restore_original_pointer(
        &mut self,
        artifacts: &mut RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        let Some(pointer) = self.original_pointer_position.take() else {
            return Ok(());
        };
        let request = DriverRequest::restore_pointer(
            self.attached.status.process_id,
            self.attached.status.application_instance_id.clone(),
            self.attached.expected_process_name.clone(),
            self.current_content_size,
            self.current_window_bounds,
            pointer,
        );
        let response = self.invoke_driver(&request, artifacts).await?;
        if !response.succeeded {
            return Err(failure(
                StressFailureClass::DriverFailure,
                None,
                response
                    .failure
                    .unwrap_or_else(|| "macOS input driver did not restore the pointer".to_owned()),
            ));
        }
        let Some(observed) = response.pointer_position else {
            return Err(failure(
                StressFailureClass::DriverFailure,
                None,
                "macOS input driver did not report the restored pointer position",
            ));
        };
        if (observed[0] - pointer[0]).abs() > 2.0 || (observed[1] - pointer[1]).abs() > 2.0 {
            return Err(failure(
                StressFailureClass::DriverFailure,
                None,
                "macOS input driver restored the pointer to an unexpected position",
            ));
        }
        artifacts
            .append_json_line(
                "controller-events.jsonl",
                &serde_json::json!({
                    "event": "pointer_restored",
                    "position": pointer,
                    "at_unix_millis": unix_millis(),
                }),
                true,
            )
            .map_err(infrastructure_failure)
    }

    fn stop_driver_and_release(
        &mut self,
        child: &mut std::process::Child,
        request: &DriverRequest,
        artifacts: &mut RunArtifacts,
    ) {
        let _ = child.kill();
        let _ = child.wait();
        self.emergency_release_after_interruption(request, artifacts);
    }

    fn emergency_release_after_interruption(
        &mut self,
        interrupted: &DriverRequest,
        artifacts: &mut RunArtifacts,
    ) {
        if interrupted.operation != crate::driver::DriverOperation::Deliver {
            return;
        }
        let request = DriverRequest::emergency_release(interrupted);
        self.driver_index += 1;
        let request_relative = format!("driver/request-{:05}.json", self.driver_index);
        let response_relative = format!("driver/response-{:05}.json", self.driver_index);
        let stderr_relative = format!("driver/stderr-{:05}.txt", self.driver_index);
        let attempt = (|| -> anyhow::Result<DriverResponse> {
            let request_path =
                artifacts.write_json(&request_relative, "emergency_release_request", &request)?;
            let response_path = artifacts.root().join(&response_relative);
            let stderr_path = artifacts.root().join(&stderr_relative);
            if let Some(parent) = stderr_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let stderr = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&stderr_path)?;
            let mut release = Command::new(&self.configuration.driver_path)
                .arg("--request")
                .arg(&request_path)
                .arg("--response")
                .arg(&response_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(stderr))
                .spawn()?;
            let deadline = Instant::now() + Duration::from_secs(6);
            loop {
                if release.try_wait()?.is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = release.kill();
                    let _ = release.wait();
                    bail!("emergency-release helper exceeded its deadline");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            artifacts.register("driver_stderr", &stderr_relative, true)?;
            let response = DriverResponse::read(&response_path, &request.request_id)?;
            artifacts.register("emergency_release_response", &response_relative, true)?;
            artifacts.append_json_line("driver-events.jsonl", &response, true)?;
            artifacts.manifest_mut().emergency_release_events = artifacts
                .manifest()
                .emergency_release_events
                .saturating_add(response.emergency_releases);
            Ok(response)
        })();
        if let Err(error) = attempt {
            let _ = artifacts.append_json_line(
                "controller-events.jsonl",
                &serde_json::json!({
                    "event": "emergency_release_failed",
                    "message": error.to_string(),
                    "at_unix_millis": unix_millis(),
                }),
                true,
            );
        }
    }

    async fn finish_app_recording(&mut self, artifacts: &mut RunArtifacts) -> anyhow::Result<()> {
        if self.configuration.dry_run {
            return Ok(());
        }
        let status = match self.attached.client.health().await {
            Ok(health) => health.recording,
            Err(error) => {
                Self::register_known_app_recording(artifacts, false)?;
                return Err(error.into());
            }
        };
        Self::update_recording_counts(artifacts, &status);
        if status.state == rgv_core::InteractionRecordingState::Recording {
            let _ = self
                .run_control(
                    vec![AutomationCommand::EndNativeObservation],
                    SHORT_CONTROL_TIMEOUT,
                    artifacts,
                )
                .await;
            let report = match self
                .run_control(
                    vec![AutomationCommand::StopInteractionRecording],
                    SHORT_CONTROL_TIMEOUT,
                    artifacts,
                )
                .await
            {
                Ok(report) => report,
                Err(error) => {
                    Self::register_known_app_recording(artifacts, false)?;
                    return Err(error);
                }
            };
            let stopped_status = report.results.first().and_then(|result| {
                serde_json::from_value::<rgv_core::InteractionRecordingStatus>(result.data.clone())
                    .ok()
            });
            if let Some(status) = &stopped_status {
                Self::update_recording_counts(artifacts, status);
            }
            let complete = stopped_status.is_some_and(|status| {
                status.state == rgv_core::InteractionRecordingState::Complete
            });
            Self::register_known_app_recording(artifacts, complete)?;
            if !complete {
                bail!("RGV did not finalise the application input recording");
            }
        } else if status.state == rgv_core::InteractionRecordingState::Complete {
            Self::register_known_app_recording(artifacts, true)?;
        } else if matches!(
            status.state,
            rgv_core::InteractionRecordingState::Incomplete
                | rgv_core::InteractionRecordingState::Failed
        ) {
            Self::register_known_app_recording(artifacts, false)?;
            bail!("RGV reported {:?} application input evidence", status.state);
        }
        Ok(())
    }

    fn update_recording_counts(
        artifacts: &mut RunArtifacts,
        status: &rgv_core::InteractionRecordingStatus,
    ) {
        artifacts.manifest_mut().coalesced_events = status.coalesced_events;
        artifacts.manifest_mut().rejected_events = status.rejected_events;
    }

    fn register_known_app_recording(
        artifacts: &mut RunArtifacts,
        complete: bool,
    ) -> anyhow::Result<()> {
        let relative = if artifacts.root().join("app-input-events.jsonl").is_file() {
            Some("app-input-events.jsonl")
        } else if artifacts
            .root()
            .join("app-input-events.partial.jsonl")
            .is_file()
        {
            Some("app-input-events.partial.jsonl")
        } else {
            None
        };
        if let Some(relative) = relative {
            artifacts.register("application_input", relative, complete)?;
        }
        Ok(())
    }

    fn retain_diagnostic_trace(
        report: &AutomationRunReport,
        artifacts: &mut RunArtifacts,
    ) -> anyhow::Result<()> {
        let Some(saved_path) = report
            .results
            .first()
            .and_then(|result| result.data.get("saved_path"))
            .and_then(serde_json::Value::as_str)
        else {
            return Ok(());
        };
        let source = PathBuf::from(saved_path);
        if !source.is_file() {
            bail!("RGV reported a diagnostic trace that does not exist");
        }
        let target = artifacts.root().join("diagnostics/trace.json");
        let same_file = source == target
            || source
                .canonicalize()
                .ok()
                .zip(target.canonicalize().ok())
                .is_some_and(|(source, target)| source == target);
        if !same_file {
            std::fs::copy(&source, &target)?;
        }
        artifacts.register("diagnostic_trace", "diagnostics/trace.json", true)
    }

    async fn preserve_failure_evidence(
        &mut self,
        failure: &StressFailureSignature,
        artifacts: &mut RunArtifacts,
    ) {
        let _ = artifacts.append_json_line(
            "controller-events.jsonl",
            &serde_json::json!({"event": "failure", "failure": failure, "at_unix_millis": unix_millis()}),
            true,
        );
        let capture = artifacts.root().join("diagnostics/failure-window.png");
        if let Ok(report) = self
            .run_control(
                vec![AutomationCommand::Snapshot {
                    path: capture.display().to_string(),
                    scope: AutomationSnapshotScope::NativeWindow,
                    format: AutomationSnapshotFormat::Png,
                }],
                Duration::from_secs(10),
                artifacts,
            )
            .await
            && report.succeeded
            && capture.is_file()
        {
            let _ = artifacts.register(
                "failure_native_window",
                "diagnostics/failure-window.png",
                true,
            );
        }
        if matches!(
            failure.class,
            StressFailureClass::UiStall | StressFailureClass::PipelineStall
        ) {
            let sample = artifacts.root().join("diagnostics/process-sample.txt");
            let _ = Command::new("/usr/bin/sample")
                .arg(self.attached.status.process_id.to_string())
                .arg("1")
                .arg("-file")
                .arg(&sample)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if sample.is_file() {
                let _ =
                    artifacts.register("process_sample", "diagnostics/process-sample.txt", true);
            }
        }
    }

    fn persist_expanded_step(
        step: &InteractionStressStep,
        artifacts: &RunArtifacts,
    ) -> Result<(), StressFailureSignature> {
        artifacts
            .append_json_line("scenario-expansion.partial.jsonl", step, true)
            .map_err(infrastructure_failure)
    }

    fn validate_requirements(
        &self,
        scenario: &InteractionStressScenario,
        workspace: &AutomationWorkspaceSnapshot,
    ) -> Result<(), StressFailureSignature> {
        if workspace.application_instance_id != self.attached.status.application_instance_id {
            return Err(failure(
                StressFailureClass::ProcessExit,
                None,
                "workspace snapshot came from an unexpected RGV instance",
            ));
        }
        if let Some(required) = &scenario.requirements.fixture
            && self.attached.fixture.as_deref() != Some(required.as_str())
        {
            return Err(failure(
                StressFailureClass::SemanticMismatch,
                None,
                format!(
                    "scenario requires fixture {required:?}, but the attach document declares {}",
                    self.attached.fixture.as_deref().unwrap_or("no fixture")
                ),
            ));
        }
        if let Some(required) = scenario.requirements.window_logical_size {
            let observed = self.current_content_size;
            if (observed[0] - f64::from(required[0])).abs() > 2.0
                || (observed[1] - f64::from(required[1])).abs() > 2.0
            {
                return Err(failure(
                    StressFailureClass::TargetGeometryChanged,
                    None,
                    format!(
                        "scenario requires a {}x{} logical window, observed {:.0}x{:.0}",
                        required[0], required[1], observed[0], observed[1]
                    ),
                ));
            }
        }
        if let Some(required) = scenario.requirements.scale_factor
            && (required - self.attached.status.scale_factor).abs() > 0.01
        {
            return Err(failure(
                StressFailureClass::TargetGeometryChanged,
                None,
                format!(
                    "scenario requires scale factor {required}, observed {}",
                    self.attached.status.scale_factor
                ),
            ));
        }
        let observed_fingerprints = workspace
            .tracks
            .iter()
            .filter_map(|track| track.source_fingerprint.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        for required in &scenario.requirements.source_fingerprints {
            if !observed_fingerprints.contains(required.as_str()) {
                return Err(failure(
                    StressFailureClass::SemanticMismatch,
                    None,
                    format!("required source fingerprint {required} is not loaded"),
                ));
            }
        }
        Ok(())
    }

    fn resolve_artifact_parent(&self) -> anyhow::Result<PathBuf> {
        let candidate = if self.configuration.artifact_parent.is_relative() {
            self.attached
                .control_root
                .join(&self.configuration.artifact_parent)
        } else {
            self.configuration.artifact_parent.clone()
        };
        std::fs::create_dir_all(&candidate)?;
        let resolved = candidate.canonicalize()?;
        if !resolved.starts_with(&self.attached.control_root) {
            bail!(
                "artifact directory {} is outside the selected RGV control root {}",
                resolved.display(),
                self.attached.control_root.display()
            );
        }
        Ok(resolved)
    }
}

fn validate_identity(
    instance_id: &str,
    process_id: u32,
    workspace: &AutomationWorkspaceSnapshot,
    health: &StressHealthSnapshot,
) -> Result<(), StressFailureSignature> {
    if workspace.application_instance_id != instance_id
        || health.application_instance_id != instance_id
        || health.process_id != process_id
    {
        return Err(failure(
            StressFailureClass::ProcessExit,
            None,
            "control status, workspace, and health identity did not agree",
        ));
    }
    Ok(())
}

fn ensure_health_identity(
    attached: &AttachedRgv,
    health: &StressHealthSnapshot,
) -> Result<(), ClassifiedRunFailure> {
    if health.application_instance_id != attached.status.application_instance_id
        || health.process_id != attached.status.process_id
    {
        Err(ClassifiedRunFailure::new(
            StressFailureClass::ProcessExit,
            "RGV process identity changed while the controller was waiting",
        ))
    } else {
        Ok(())
    }
}

fn resolve_target_rect(
    target: &StressTarget,
    workspace: &AutomationWorkspaceSnapshot,
    content_size: [f64; 2],
) -> Result<AutomationRectSnapshot, StressFailureSignature> {
    match target {
        StressTarget::Window => Ok(AutomationRectSnapshot {
            left: 0.0,
            top: 0.0,
            right: content_size[0] as f32,
            bottom: content_size[1] as f32,
        }),
        StressTarget::WindowCoordinate {
            required_logical_size,
        } => {
            if (content_size[0] - f64::from(required_logical_size[0])).abs() > 2.0
                || (content_size[1] - f64::from(required_logical_size[1])).abs() > 2.0
            {
                Err(failure(
                    StressFailureClass::TargetGeometryChanged,
                    None,
                    "fragile window-coordinate target no longer matches its required size",
                ))
            } else {
                Ok(AutomationRectSnapshot {
                    left: 0.0,
                    top: 0.0,
                    right: content_size[0] as f32,
                    bottom: content_size[1] as f32,
                })
            }
        }
        StressTarget::Accessibility { identifier } => Err(failure(
            StressFailureClass::DriverFailure,
            None,
            format!("accessibility target {identifier} is not exposed by this build"),
        )),
        _ => target.resolve_rect(workspace).ok_or_else(|| {
            failure(
                StressFailureClass::TargetGeometryChanged,
                None,
                format!(
                    "semantic target is not present in workspace revision {}",
                    workspace.workspace_revision
                ),
            )
        }),
    }
}

#[allow(clippy::too_many_lines)]
fn validate_native_shape(
    step_id: &str,
    events: &[rgv_core::StressNativeEvent],
    summary: &NativeObservationSummary,
) -> Result<(), StressFailureSignature> {
    if summary.extraneous_events > 0 {
        return Err(failure(
            StressFailureClass::ExtraneousNativeInput,
            Some(step_id.to_owned()),
            format!(
                "RGV observed {} additional interaction lifecycle(s) during exclusive replay",
                summary.extraneous_events
            ),
        ));
    }
    if summary.received_events == 0 || summary.applied_events == 0 || !summary.target_acquired {
        return Err(failure(
            StressFailureClass::NativeEventMissing,
            Some(step_id.to_owned()),
            "RGV did not receive and apply native input for the intended target",
        ));
    }
    let expected_shape = expected_event_shape(events);
    for (label, expected, observed) in [
        (
            "primary_down",
            expected_shape.primary_down_events,
            summary.event_shape.primary_down_events,
        ),
        (
            "primary_up",
            expected_shape.primary_up_events,
            summary.event_shape.primary_up_events,
        ),
        (
            "secondary_down",
            expected_shape.secondary_down_events,
            summary.event_shape.secondary_down_events,
        ),
        (
            "secondary_up",
            expected_shape.secondary_up_events,
            summary.event_shape.secondary_up_events,
        ),
        (
            "scroll_began",
            expected_shape.scroll_began_events,
            summary.event_shape.scroll_began_events,
        ),
        (
            "scroll_ended",
            expected_shape.scroll_ended_events,
            summary.event_shape.scroll_ended_events,
        ),
        (
            "scroll_cancelled",
            expected_shape.scroll_cancelled_events,
            summary.event_shape.scroll_cancelled_events,
        ),
        (
            "key_down",
            expected_shape.key_down_events,
            summary.event_shape.key_down_events,
        ),
        (
            "key_up",
            expected_shape.key_up_events,
            summary.event_shape.key_up_events,
        ),
        (
            "focus",
            expected_shape.focus_events,
            summary.event_shape.focus_events,
        ),
        (
            "resize",
            expected_shape.resize_events,
            summary.event_shape.resize_events,
        ),
    ] {
        if observed < expected {
            return Err(failure(
                StressFailureClass::NativeEventShapeMismatch,
                Some(step_id.to_owned()),
                format!("RGV observed {observed} {label} event(s), expected {expected}"),
            ));
        }
        if observed > expected {
            return Err(failure(
                StressFailureClass::ExtraneousNativeInput,
                Some(step_id.to_owned()),
                format!("RGV observed {observed} {label} event(s), expected {expected}"),
            ));
        }
    }
    for (label, expected, observed) in [
        (
            "primary_drag",
            expected_shape.primary_drag_events,
            summary.event_shape.primary_drag_events,
        ),
        (
            "secondary_drag",
            expected_shape.secondary_drag_events,
            summary.event_shape.secondary_drag_events,
        ),
        (
            "unphased_scroll",
            expected_shape.scroll_none_events,
            summary.event_shape.scroll_none_events,
        ),
        (
            "scroll_changed",
            expected_shape.scroll_changed_events,
            summary.event_shape.scroll_changed_events,
        ),
        (
            "scroll_may_begin",
            expected_shape.scroll_may_begin_events,
            summary.event_shape.scroll_may_begin_events,
        ),
    ] {
        if expected > 0 && observed == 0 {
            return Err(failure(
                StressFailureClass::NativeEventShapeMismatch,
                Some(step_id.to_owned()),
                format!("RGV did not observe the requested {label} event structure"),
            ));
        }
    }
    if summary.event_shape.direction_reversals < expected_shape.direction_reversals {
        return Err(failure(
            StressFailureClass::NativeEventShapeMismatch,
            Some(step_id.to_owned()),
            format!(
                "RGV observed {} direction reversal(s), expected at least {}",
                summary.event_shape.direction_reversals, expected_shape.direction_reversals
            ),
        ));
    }
    let (requires_begin, requires_advance, requires_termination) = required_lifecycle_shape(events);
    if requires_termination && !summary.terminated {
        return Err(failure(
            StressFailureClass::InteractionLock,
            Some(step_id.to_owned()),
            "RGV received the gesture but its interaction did not terminate",
        ));
    }
    if (requires_begin && !summary.began) || (requires_advance && !summary.advanced) {
        return Err(failure(
            StressFailureClass::NativeEventShapeMismatch,
            Some(step_id.to_owned()),
            "RGV received input but did not observe the required begin/advance/termination structure",
        ));
    }
    Ok(())
}

fn required_lifecycle_shape(events: &[rgv_core::StressNativeEvent]) -> (bool, bool, bool) {
    let mut requires_begin = false;
    let mut requires_advance = false;
    let mut requires_termination = false;
    let mut interaction_active = false;
    for event in events {
        match event {
            rgv_core::StressNativeEvent::PrimaryDown { .. }
            | rgv_core::StressNativeEvent::SecondaryDown { .. }
            | rgv_core::StressNativeEvent::Scroll {
                phase: rgv_core::StressTouchPhase::Began,
                ..
            } => {
                requires_begin = true;
                interaction_active = true;
            }
            rgv_core::StressNativeEvent::PrimaryDrag { .. }
            | rgv_core::StressNativeEvent::SecondaryDrag { .. }
            | rgv_core::StressNativeEvent::Scroll {
                phase: rgv_core::StressTouchPhase::Changed,
                ..
            } => {
                requires_advance = true;
                interaction_active = true;
            }
            rgv_core::StressNativeEvent::PrimaryUp { .. }
            | rgv_core::StressNativeEvent::SecondaryUp { .. }
            | rgv_core::StressNativeEvent::Scroll {
                phase: rgv_core::StressTouchPhase::Ended | rgv_core::StressTouchPhase::Cancelled,
                ..
            } => {
                requires_termination = true;
                interaction_active = false;
            }
            rgv_core::StressNativeEvent::Focus { focused: false, .. } => {
                requires_termination |= interaction_active;
                interaction_active = false;
            }
            _ => {}
        }
    }
    (requires_begin, requires_advance, requires_termination)
}

#[allow(clippy::too_many_lines)]
fn expected_event_shape(events: &[rgv_core::StressNativeEvent]) -> NativeEventShape {
    let mut shape = NativeEventShape::default();
    let mut last_motion = None;
    let mut motion_direction = [0_i8; 2];
    let mut scroll_direction = [0_i8; 2];
    let mut primary_held = false;
    let mut secondary_held = false;
    let mut keys_held = std::collections::BTreeSet::new();
    for event in events {
        match event {
            rgv_core::StressNativeEvent::Move { position, .. } => {
                shape.move_events = shape.move_events.saturating_add(1);
                observe_expected_motion(
                    &mut shape,
                    &mut last_motion,
                    &mut motion_direction,
                    [position.x, position.y],
                );
            }
            rgv_core::StressNativeEvent::PrimaryDown { .. } => {
                shape.primary_down_events = shape.primary_down_events.saturating_add(1);
                primary_held = true;
            }
            rgv_core::StressNativeEvent::PrimaryDrag { position, .. } => {
                shape.primary_drag_events = shape.primary_drag_events.saturating_add(1);
                observe_expected_motion(
                    &mut shape,
                    &mut last_motion,
                    &mut motion_direction,
                    [position.x, position.y],
                );
            }
            rgv_core::StressNativeEvent::PrimaryUp { .. } => {
                shape.primary_up_events = shape.primary_up_events.saturating_add(1);
                primary_held = false;
            }
            rgv_core::StressNativeEvent::SecondaryDown { .. } => {
                shape.secondary_down_events = shape.secondary_down_events.saturating_add(1);
                secondary_held = true;
            }
            rgv_core::StressNativeEvent::SecondaryDrag { position, .. } => {
                shape.secondary_drag_events = shape.secondary_drag_events.saturating_add(1);
                observe_expected_motion(
                    &mut shape,
                    &mut last_motion,
                    &mut motion_direction,
                    [position.x, position.y],
                );
            }
            rgv_core::StressNativeEvent::SecondaryUp { .. } => {
                shape.secondary_up_events = shape.secondary_up_events.saturating_add(1);
                secondary_held = false;
            }
            rgv_core::StressNativeEvent::Scroll {
                delta_x,
                delta_y,
                phase,
                ..
            } => {
                let counter = match phase {
                    rgv_core::StressTouchPhase::None => &mut shape.scroll_none_events,
                    rgv_core::StressTouchPhase::Began => &mut shape.scroll_began_events,
                    rgv_core::StressTouchPhase::Changed => &mut shape.scroll_changed_events,
                    rgv_core::StressTouchPhase::Ended => &mut shape.scroll_ended_events,
                    rgv_core::StressTouchPhase::Cancelled => &mut shape.scroll_cancelled_events,
                    rgv_core::StressTouchPhase::MayBegin => &mut shape.scroll_may_begin_events,
                };
                *counter = counter.saturating_add(1);
                shape.direction_reversals = shape
                    .direction_reversals
                    .saturating_add(observe_direction(&mut scroll_direction[0], *delta_x))
                    .saturating_add(observe_direction(&mut scroll_direction[1], *delta_y));
            }
            rgv_core::StressNativeEvent::KeyDown { key_code, .. } => {
                shape.key_down_events = shape.key_down_events.saturating_add(1);
                keys_held.insert(*key_code);
            }
            rgv_core::StressNativeEvent::KeyUp { key_code, .. } => {
                shape.key_up_events = shape.key_up_events.saturating_add(1);
                keys_held.remove(key_code);
            }
            rgv_core::StressNativeEvent::Focus { focused, .. } => {
                shape.focus_events = shape.focus_events.saturating_add(1);
                if !focused {
                    shape.primary_up_events = shape
                        .primary_up_events
                        .saturating_add(u64::from(primary_held));
                    shape.secondary_up_events = shape
                        .secondary_up_events
                        .saturating_add(u64::from(secondary_held));
                    shape.key_up_events =
                        shape.key_up_events.saturating_add(keys_held.len() as u64);
                    primary_held = false;
                    secondary_held = false;
                    keys_held.clear();
                }
            }
            rgv_core::StressNativeEvent::ResizeWindow { .. } => {
                shape.resize_events = shape.resize_events.saturating_add(1);
            }
            rgv_core::StressNativeEvent::Unsupported { .. } => {}
        }
    }
    shape
}

fn observe_expected_motion(
    shape: &mut NativeEventShape,
    previous: &mut Option<[f64; 2]>,
    direction: &mut [i8; 2],
    position: [f64; 2],
) {
    if let Some(previous_position) = *previous {
        shape.direction_reversals = shape
            .direction_reversals
            .saturating_add(observe_direction(
                &mut direction[0],
                position[0] - previous_position[0],
            ))
            .saturating_add(observe_direction(
                &mut direction[1],
                position[1] - previous_position[1],
            ));
    }
    *previous = Some(position);
}

fn observe_direction(previous: &mut i8, delta: f64) -> u64 {
    let next = if delta > f64::EPSILON {
        1
    } else if delta < -f64::EPSILON {
        -1
    } else {
        return 0;
    };
    let reversed = *previous != 0 && *previous != next;
    *previous = next;
    u64::from(reversed)
}

fn flatten_repeats(steps: &[InteractionStressStep]) -> Vec<InteractionStressStep> {
    let mut repeat_serial = 0_u64;
    let mut flattened = flatten_repeats_inner(steps, &mut repeat_serial);
    rewrite_external_repeat_references(&mut flattened);
    flattened
}

fn flatten_repeats_inner(
    steps: &[InteractionStressStep],
    repeat_serial: &mut u64,
) -> Vec<InteractionStressStep> {
    let mut flattened = Vec::new();
    for step in steps {
        match step {
            InteractionStressStep::Repeat { count, steps } => {
                let block = *repeat_serial;
                *repeat_serial = repeat_serial.saturating_add(1);
                for iteration in 0..*count {
                    let mut nested = flatten_repeats_inner(steps, repeat_serial);
                    suffix_repeat_scope(&mut nested, &format!("@repeat-{block}-{iteration}"));
                    flattened.extend(nested);
                }
            }
            other => flattened.push(other.clone()),
        }
    }
    flattened
}

fn suffix_repeat_scope(steps: &mut [InteractionStressStep], suffix: &str) {
    let ids = steps
        .iter()
        .filter_map(|step| match step {
            InteractionStressStep::Gesture { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    for step in steps {
        match step {
            InteractionStressStep::Gesture { id, .. } if ids.contains(id) => {
                id.push_str(suffix);
            }
            InteractionStressStep::Assert { predicate }
            | InteractionStressStep::Barrier { predicate, .. } => match predicate {
                StressPredicate::GestureTerminated { step_id }
                | StressPredicate::NativeRoutingComplete { step_id }
                    if ids.contains(step_id) =>
                {
                    step_id.push_str(suffix);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn rewrite_external_repeat_references(steps: &mut [InteractionStressStep]) {
    let exact_ids = steps
        .iter()
        .filter_map(|step| match step {
            InteractionStressStep::Gesture { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut last_expansion = BTreeMap::new();
    for id in &exact_ids {
        if let Some((base, _)) = id.split_once("@repeat-") {
            last_expansion.insert(base.to_owned(), id.clone());
        }
    }
    for step in steps {
        let (InteractionStressStep::Assert { predicate }
        | InteractionStressStep::Barrier { predicate, .. }) = step
        else {
            continue;
        };
        let (StressPredicate::GestureTerminated { step_id }
        | StressPredicate::NativeRoutingComplete { step_id }) = predicate
        else {
            continue;
        };
        if !exact_ids.contains(step_id)
            && let Some(expanded) = last_expansion.get(step_id)
        {
            step_id.clone_from(expanded);
        }
    }
}

const fn step_kind(step: &InteractionStressStep) -> &'static str {
    match step {
        InteractionStressStep::Control { .. } => "control",
        InteractionStressStep::Gesture { .. } => "gesture",
        InteractionStressStep::Barrier { .. } => "barrier",
        InteractionStressStep::Assert { .. } => "assert",
        InteractionStressStep::Checkpoint { .. } => "checkpoint",
        InteractionStressStep::Generated { .. } => "generated",
        InteractionStressStep::Repeat { .. } => "repeat",
    }
}

fn mix_seed(seed: u64, index: u64) -> u64 {
    let mut value = seed ^ index.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

#[allow(clippy::needless_pass_by_value)]
fn infrastructure_failure(error: anyhow::Error) -> StressFailureSignature {
    if error.downcast_ref::<RunCancelled>().is_some() {
        return failure(
            StressFailureClass::Cancelled,
            None,
            "stress run was cancelled",
        );
    }
    if let Some(classified) = error.downcast_ref::<ClassifiedRunFailure>() {
        return failure(classified.class, None, classified.message.clone());
    }
    failure(StressFailureClass::DriverFailure, None, error.to_string())
}

fn failure(
    class: StressFailureClass,
    step_id: Option<String>,
    message: impl Into<String>,
) -> StressFailureSignature {
    StressFailureSignature {
        class,
        step_id,
        target: None,
        owner: None,
        message: message.into(),
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

trait Pipe: Sized {
    fn pipe<T>(self, function: impl FnOnce(Self) -> T) -> T {
        function(self)
    }
}

impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use rgv_core::{
        InteractionStressStep, StressModifiers, StressNativeEvent, StressPoint, StressTarget,
        StressTiming,
    };

    use super::*;

    fn health(ui: u64, worker: u64, desired: u64, committed: u64) -> StressHealthSnapshot {
        StressHealthSnapshot {
            application_instance_id: "instance".to_owned(),
            process_id: 7,
            ui_heartbeat_sequence: ui,
            worker_progress_sequence: worker,
            desired_generation: desired,
            committed_generation: committed,
            ..StressHealthSnapshot::default()
        }
    }

    #[test]
    fn health_watchdog_distinguishes_ui_pipeline_and_churn_failures() {
        let start = Instant::now();
        let mut ui_watchdog = HealthWatchdog::new(health(1, 0, 0, 0), start);
        let ui_failure = ui_watchdog
            .observe(
                health(1, 0, 0, 0),
                start + Duration::from_secs(6),
                Duration::from_secs(5),
                Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(ui_failure.class, StressFailureClass::UiStall);

        let mut pipeline_watchdog = HealthWatchdog::new(health(1, 0, 2, 1), start);
        let pipeline_failure = pipeline_watchdog
            .observe(
                health(2, 0, 2, 1),
                start + Duration::from_secs(6),
                Duration::from_secs(30),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(pipeline_failure.class, StressFailureClass::PipelineStall);

        let mut churn_watchdog = HealthWatchdog::new(health(1, 0, 2, 1), start);
        assert!(
            churn_watchdog
                .observe(
                    health(2, 1, 2, 1),
                    start + Duration::from_secs(6),
                    Duration::from_secs(30),
                    Duration::from_secs(5),
                )
                .is_none()
        );
        let churn_failure = churn_watchdog
            .observe(
                health(3, 2, 2, 1),
                start + Duration::from_secs(11),
                Duration::from_secs(30),
                Duration::from_secs(5),
            )
            .unwrap();
        assert_eq!(churn_failure.class, StressFailureClass::ProgressChurn);
    }

    #[test]
    fn native_shape_allows_motion_coalescing_but_requires_lifecycle_boundaries() {
        let events = vec![
            StressNativeEvent::PrimaryDown {
                at_micros: 0,
                modifiers: StressModifiers::default(),
            },
            StressNativeEvent::PrimaryDrag {
                at_micros: 1,
                position: StressPoint { x: 0.5, y: 0.5 },
                modifiers: StressModifiers::default(),
            },
            StressNativeEvent::PrimaryDrag {
                at_micros: 2,
                position: StressPoint { x: 0.8, y: 0.5 },
                modifiers: StressModifiers::default(),
            },
            StressNativeEvent::PrimaryUp {
                at_micros: 3,
                modifiers: StressModifiers::default(),
            },
        ];
        let summary = NativeObservationSummary {
            received_events: 3,
            applied_events: 3,
            target_acquired: true,
            began: true,
            advanced: true,
            terminated: true,
            event_shape: expected_event_shape(&events),
            ..NativeObservationSummary::default()
        };
        assert!(validate_native_shape("step", &events, &summary).is_ok());
        let mut not_applied = summary.clone();
        not_applied.applied_events = 0;
        assert_eq!(
            validate_native_shape("step", &events, &not_applied)
                .unwrap_err()
                .class,
            StressFailureClass::NativeEventMissing
        );
        assert!(
            validate_native_shape(
                "step",
                &events,
                &NativeObservationSummary {
                    terminated: false,
                    ..summary
                }
            )
            .is_err()
        );
    }

    #[test]
    fn standalone_focus_cycle_does_not_invent_an_interaction_termination() {
        let events = vec![
            StressNativeEvent::Focus {
                at_micros: 0,
                focused: false,
            },
            StressNativeEvent::Focus {
                at_micros: 1,
                focused: true,
            },
        ];
        let summary = NativeObservationSummary {
            received_events: 2,
            applied_events: 2,
            target_acquired: true,
            event_shape: expected_event_shape(&events),
            ..NativeObservationSummary::default()
        };
        assert!(validate_native_shape("focus-cycle", &events, &summary).is_ok());
        assert_eq!(required_lifecycle_shape(&events), (false, false, false));
    }

    #[test]
    fn repeat_expansion_preserves_order() {
        let gesture = InteractionStressStep::Gesture {
            id: "g".to_owned(),
            target: StressTarget::Window,
            fragile_target: false,
            timing: StressTiming::Recorded,
            events: vec![StressNativeEvent::Focus {
                at_micros: 0,
                focused: true,
            }],
        };
        let flattened = flatten_repeats(&[InteractionStressStep::Repeat {
            count: 3,
            steps: vec![gesture],
        }]);
        assert_eq!(flattened.len(), 3);
        let ids = flattened
            .iter()
            .filter_map(|step| match step {
                InteractionStressStep::Gesture { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 3);
    }
}
