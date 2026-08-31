use super::super::*;

pub(crate) struct AutomationRunner {
    pub(crate) commands: VecDeque<(usize, AutomationCommand)>,
    pub(crate) results: Vec<AutomationCommandResult>,
    pub(crate) waiting: Option<AutomationWait>,
    pub(crate) report_path: Option<PathBuf>,
    pub(crate) report_worker: Option<JoinHandle<std::result::Result<(), String>>>,
    pub(crate) control_reply: Option<std::sync::mpsc::SyncSender<AutomationRunReport>>,
    pub(crate) exit_after_report: bool,
    pub(crate) finished: bool,
    pub(crate) run_started: Instant,
    pub(crate) process_started: Option<Instant>,
    pub(crate) active_command: Option<ActiveAutomationCommand>,
    pub(crate) first_frame_ms: Option<f64>,
    pub(crate) first_settled_ms: Option<f64>,
    pub(crate) environment: AutomationEnvironmentTelemetry,
    pub(crate) presented_frames: u64,
    pub(crate) frame_samples_ms: Vec<f64>,
    pub(crate) frame_samples_dropped: u64,
    pub(crate) gpu: AutomationGpuTelemetry,
    pub(crate) navigation: Vec<AutomationNavigationTelemetry>,
    pending_navigation: Option<PendingAutomationNavigation>,
}

pub(crate) struct ActiveAutomationCommand {
    pub(crate) index: usize,
    pub(crate) name: &'static str,
    pub(crate) started: Instant,
}

const MAXIMUM_AUTOMATION_NAVIGATION_SAMPLES: usize = 1_024;

#[derive(Clone, Debug, Default)]
pub(crate) struct AutomationNavigationState {
    pub(crate) source_acquisitions: u64,
    pub(crate) foreground_submissions: u64,
    pub(crate) speculation_submissions: u64,
    pub(crate) scene_phase: String,
    pub(crate) camera_only_updates: u64,
    pub(crate) scene_publications: u64,
}

struct PendingAutomationNavigation {
    panel_id: PanelId,
    interval: GenomicInterval,
    started: Instant,
    source_acquisitions_before: u64,
    speculation_submissions_before: u64,
    first_presented_frame_ms: Option<f64>,
    presented_frames: u64,
    maximum_frame_ms: Option<f64>,
    gpu_uploaded_bytes: u64,
    gpu_buffer_reallocations: u64,
    gpu_slab_cache_hits: u64,
}

#[derive(Clone)]
pub(crate) enum AutomationWait {
    Settled {
        command_index: usize,
        deadline: Instant,
    },
    File {
        command_index: usize,
    },
    Reference {
        command_index: usize,
        deadline: Instant,
    },
    AlignmentTrack {
        command_index: usize,
        track_id: TrackId,
        deadline: Instant,
    },
    AuxiliaryTrack {
        command_index: usize,
        track_id: TrackId,
        deadline: Instant,
    },
    ExactBaseSelection {
        command_index: usize,
        panel_id: PanelId,
        track_id: TrackId,
        position: u64,
        previous_selection: Option<(String, u64)>,
        deadline: Instant,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AutomationAuxiliaryOpenState {
    Pending,
    Ready,
    Failed(String),
}

pub(crate) enum AutomationExecution {
    Complete {
        message: String,
        data: serde_json::Value,
    },
    Wait(AutomationWait),
    Failed {
        code: AutomationErrorCode,
        message: String,
    },
}

impl AutomationRunner {
    #[cfg(test)]
    pub(crate) fn new(batch: AutomationBatch, report_path: PathBuf) -> Self {
        Self::new_with_telemetry(
            batch,
            report_path,
            None,
            AutomationEnvironmentTelemetry::default(),
        )
    }

    pub(crate) fn new_with_telemetry(
        batch: AutomationBatch,
        report_path: PathBuf,
        process_started: Option<Instant>,
        environment: AutomationEnvironmentTelemetry,
    ) -> Self {
        Self {
            commands: batch.commands.into_iter().enumerate().collect(),
            results: Vec::new(),
            waiting: None,
            report_path: Some(report_path),
            report_worker: None,
            control_reply: None,
            exit_after_report: false,
            finished: false,
            run_started: Instant::now(),
            process_started,
            active_command: None,
            first_frame_ms: None,
            first_settled_ms: None,
            environment,
            presented_frames: 0,
            frame_samples_ms: Vec::new(),
            frame_samples_dropped: 0,
            gpu: AutomationGpuTelemetry::default(),
            navigation: Vec::new(),
            pending_navigation: None,
        }
    }

    pub(crate) fn for_control(request: ControlRequest) -> Self {
        Self {
            commands: request.batch.commands.into_iter().enumerate().collect(),
            results: Vec::new(),
            waiting: None,
            report_path: None,
            report_worker: None,
            control_reply: Some(request.reply),
            exit_after_report: false,
            finished: false,
            run_started: Instant::now(),
            process_started: None,
            active_command: None,
            first_frame_ms: None,
            first_settled_ms: None,
            environment: AutomationEnvironmentTelemetry::default(),
            presented_frames: 0,
            frame_samples_ms: Vec::new(),
            frame_samples_dropped: 0,
            gpu: AutomationGpuTelemetry::default(),
            navigation: Vec::new(),
            pending_navigation: None,
        }
    }

    pub(crate) fn begin_command(&mut self, index: usize, name: &'static str, now: Instant) {
        self.active_command = Some(ActiveAutomationCommand {
            index,
            name,
            started: now,
        });
    }

    pub(crate) fn record_first_settled(&mut self, now: Instant) {
        if self.first_settled_ms.is_none()
            && let Some(process_started) = self.process_started
        {
            self.first_settled_ms = Some(milliseconds_between(process_started, now));
        }
    }

    pub(crate) fn record_frame(
        &mut self,
        now: Instant,
        frame_ms: f64,
        prepared: Option<PrepareStats>,
        surface: [u32; 2],
        scale_factor: f64,
    ) {
        self.presented_frames = self.presented_frames.saturating_add(1);
        if self.frame_samples_ms.len() < MAX_AUTOMATION_FRAME_SAMPLES {
            self.frame_samples_ms.push(frame_ms);
        } else {
            self.frame_samples_dropped = self.frame_samples_dropped.saturating_add(1);
        }
        self.environment.surface_width_pixels = surface[0];
        self.environment.surface_height_pixels = surface[1];
        self.environment.scale_factor = scale_factor;
        if self.first_frame_ms.is_none()
            && let Some(process_started) = self.process_started
        {
            self.first_frame_ms = Some(milliseconds_between(process_started, now));
        }
        let Some(prepared) = prepared else {
            return;
        };
        self.gpu.prepared_frames = self.gpu.prepared_frames.saturating_add(1);
        self.gpu.peak_instances = self.gpu.peak_instances.max(prepared.total_instances);
        self.gpu.peak_uploaded_bytes = self.gpu.peak_uploaded_bytes.max(prepared.uploaded_bytes);
        self.gpu.total_uploaded_bytes = self
            .gpu
            .total_uploaded_bytes
            .saturating_add(prepared.uploaded_bytes as u64);
        self.gpu.uploaded_frames = self
            .gpu
            .uploaded_frames
            .saturating_add(u64::from(prepared.uploaded_bytes > 0));
        self.gpu.buffer_reallocations = self
            .gpu
            .buffer_reallocations
            .saturating_add(u64::from(prepared.buffer_reallocated));
        self.gpu.truncated_frames = self
            .gpu
            .truncated_frames
            .saturating_add(u64::from(prepared.truncated));
        self.gpu.slab_cache_hits = self
            .gpu
            .slab_cache_hits
            .saturating_add(u64::from(prepared.slab_cache_hit));
        self.gpu.peak_slab_entries = self.gpu.peak_slab_entries.max(prepared.slab_entries);
        self.gpu.peak_slab_resident_bytes = self
            .gpu
            .peak_slab_resident_bytes
            .max(prepared.slab_resident_bytes);
    }

    pub(crate) fn begin_navigation(
        &mut self,
        panel_id: PanelId,
        interval: GenomicInterval,
        state: &AutomationNavigationState,
        now: Instant,
    ) {
        if let Some(previous) = self.pending_navigation.take() {
            self.push_navigation(previous.finish(now, state, false));
        }
        self.pending_navigation = Some(PendingAutomationNavigation {
            panel_id,
            interval,
            started: now,
            source_acquisitions_before: state.source_acquisitions,
            speculation_submissions_before: state.speculation_submissions,
            first_presented_frame_ms: None,
            presented_frames: 0,
            maximum_frame_ms: None,
            gpu_uploaded_bytes: 0,
            gpu_buffer_reallocations: 0,
            gpu_slab_cache_hits: 0,
        });
    }

    pub(crate) fn pending_navigation_target(&self) -> Option<(PanelId, GenomicInterval)> {
        self.pending_navigation
            .as_ref()
            .map(|pending| (pending.panel_id, pending.interval))
    }

    pub(crate) fn record_navigation_frame(
        &mut self,
        now: Instant,
        frame_ms: f64,
        prepared: Option<PrepareStats>,
        authoritative: bool,
        state: &AutomationNavigationState,
    ) {
        let Some(pending) = self.pending_navigation.as_mut() else {
            return;
        };
        pending.presented_frames = pending.presented_frames.saturating_add(1);
        pending.first_presented_frame_ms.get_or_insert_with(|| {
            now.saturating_duration_since(pending.started).as_secs_f64() * 1_000.0
        });
        pending.maximum_frame_ms = Some(
            pending
                .maximum_frame_ms
                .map_or(frame_ms, |maximum| maximum.max(frame_ms)),
        );
        if let Some(prepared) = prepared {
            pending.gpu_uploaded_bytes = pending
                .gpu_uploaded_bytes
                .saturating_add(prepared.uploaded_bytes as u64);
            pending.gpu_buffer_reallocations = pending
                .gpu_buffer_reallocations
                .saturating_add(u64::from(prepared.buffer_reallocated));
            pending.gpu_slab_cache_hits = pending
                .gpu_slab_cache_hits
                .saturating_add(u64::from(prepared.slab_cache_hit));
        }
        if authoritative {
            let completed = self
                .pending_navigation
                .take()
                .expect("pending navigation exists");
            self.push_navigation(completed.finish(now, state, true));
        }
    }

    fn push_navigation(&mut self, sample: AutomationNavigationTelemetry) {
        if self.navigation.len() == MAXIMUM_AUTOMATION_NAVIGATION_SAMPLES {
            self.navigation.remove(0);
        }
        self.navigation.push(sample);
    }

    pub(crate) fn report(&self, now: Instant) -> AutomationRunReport {
        let (median_ms, p95_ms, maximum_ms) = frame_summary(&self.frame_samples_ms);
        AutomationRunReport {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            succeeded: self
                .results
                .iter()
                .all(|result| result.status == AutomationCommandStatus::Succeeded),
            results: self.results.clone(),
            telemetry: AutomationRunTelemetry {
                run_elapsed_ms: milliseconds_between(self.run_started, now),
                startup_to_first_frame_ms: self.first_frame_ms,
                startup_to_first_settled_ms: self.first_settled_ms,
                environment: self.environment.clone(),
                frames: AutomationFrameTelemetry {
                    presented_frames: self.presented_frames,
                    samples_ms: self.frame_samples_ms.clone(),
                    samples_dropped: self.frame_samples_dropped,
                    median_ms,
                    p95_ms,
                    maximum_ms,
                },
                gpu: self.gpu.clone(),
                navigation: self.navigation.clone(),
            },
        }
    }

    pub(crate) fn finish_command(
        &mut self,
        command_index: usize,
        outcome: std::result::Result<(String, serde_json::Value), (AutomationErrorCode, String)>,
    ) -> bool {
        if self
            .active_command
            .as_ref()
            .is_some_and(|active| active.index != command_index)
        {
            return false;
        }
        if let Some(waiting_index) = self.waiting.as_ref().map(|waiting| match waiting {
            AutomationWait::Settled { command_index, .. }
            | AutomationWait::File { command_index }
            | AutomationWait::Reference { command_index, .. }
            | AutomationWait::AlignmentTrack { command_index, .. }
            | AutomationWait::AuxiliaryTrack { command_index, .. }
            | AutomationWait::ExactBaseSelection { command_index, .. } => *command_index,
        }) && waiting_index != command_index
        {
            return false;
        }
        self.waiting = None;
        let active = self.active_command.take();
        let command = active.as_ref().map_or("", |active| active.name).to_owned();
        let elapsed_ms = active.map_or(0.0, |active| {
            active.started.elapsed().as_secs_f64() * 1_000.0
        });
        match outcome {
            Ok((message, data)) => self.results.push(AutomationCommandResult {
                command_index,
                command,
                status: AutomationCommandStatus::Succeeded,
                error_code: None,
                message,
                data,
                elapsed_ms,
            }),
            Err((code, message)) => {
                self.results.push(AutomationCommandResult {
                    command_index,
                    command,
                    status: AutomationCommandStatus::Failed,
                    error_code: Some(code),
                    message,
                    data: serde_json::Value::Null,
                    elapsed_ms,
                });
                for (index, skipped) in self.commands.drain(..) {
                    self.results.push(AutomationCommandResult {
                        command_index: index,
                        command: skipped.name().to_owned(),
                        status: AutomationCommandStatus::Skipped,
                        error_code: None,
                        message: "Skipped after an earlier command failed".to_owned(),
                        data: serde_json::Value::Null,
                        elapsed_ms: 0.0,
                    });
                }
            }
        }
        true
    }
}

impl PendingAutomationNavigation {
    fn finish(
        self,
        now: Instant,
        state: &AutomationNavigationState,
        complete: bool,
    ) -> AutomationNavigationTelemetry {
        AutomationNavigationTelemetry {
            panel_id: self.panel_id.get(),
            start: self.interval.start(),
            end: self.interval.end(),
            input_to_first_presented_frame_ms: self.first_presented_frame_ms,
            input_to_authoritative_frame_ms: complete
                .then(|| now.saturating_duration_since(self.started).as_secs_f64() * 1_000.0),
            presented_frames: self.presented_frames,
            maximum_frame_ms: self.maximum_frame_ms,
            scene_phase: state.scene_phase.clone(),
            foreground_submissions: state.foreground_submissions,
            speculation_submissions: state
                .speculation_submissions
                .saturating_sub(self.speculation_submissions_before),
            source_acquisitions: state
                .source_acquisitions
                .saturating_sub(self.source_acquisitions_before),
            camera_only_updates: state.camera_only_updates,
            scene_publications: state.scene_publications,
            gpu_uploaded_bytes: self.gpu_uploaded_bytes,
            gpu_buffer_reallocations: self.gpu_buffer_reallocations,
            gpu_slab_cache_hits: self.gpu_slab_cache_hits,
            complete,
        }
    }
}

pub(crate) fn milliseconds_between(start: Instant, end: Instant) -> f64 {
    end.saturating_duration_since(start).as_secs_f64() * 1_000.0
}

pub(crate) fn frame_summary(samples: &[f64]) -> (Option<f64>, Option<f64>, Option<f64>) {
    if samples.is_empty() {
        return (None, None, None);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let p95_index = ((sorted.len() as f64 * 0.95).ceil() as usize).clamp(1, sorted.len()) - 1;
    (
        Some(median),
        Some(sorted[p95_index]),
        sorted.last().copied(),
    )
}

pub(crate) fn automation_failure(
    code: AutomationErrorCode,
    error: impl std::fmt::Display,
) -> AutomationExecution {
    AutomationExecution::Failed {
        code,
        message: error.to_string(),
    }
}

pub(crate) fn automation_bool(value: &serde_json::Value, key: &str) -> Result<bool> {
    value
        .as_bool()
        .with_context(|| format!("{key} must be a boolean"))
}

pub(crate) fn automation_u64(value: &serde_json::Value, key: &str) -> Result<u64> {
    value
        .as_u64()
        .with_context(|| format!("{key} must be a non-negative integer"))
}

pub(crate) fn automation_optional_u64(value: &serde_json::Value, key: &str) -> Result<Option<u64>> {
    if value.is_null() {
        Ok(None)
    } else {
        automation_u64(value, key).map(Some)
    }
}

pub(crate) fn automation_u8(value: u64, key: &str) -> Result<u8> {
    u8::try_from(value).with_context(|| format!("{key} must be no greater than {}", u8::MAX))
}

pub(crate) fn automation_u32(value: u64, key: &str) -> Result<u32> {
    u32::try_from(value).with_context(|| format!("{key} must be no greater than {}", u32::MAX))
}

pub(crate) fn automation_f32(value: &serde_json::Value, key: &str) -> Result<f32> {
    let number = value
        .as_f64()
        .with_context(|| format!("{key} must be a number"))?;
    anyhow::ensure!(number.is_finite(), "{key} must be finite");
    anyhow::ensure!(
        number >= f64::from(f32::MIN) && number <= f64::from(f32::MAX),
        "{key} is outside the supported numeric range"
    );
    Ok(number as f32)
}

pub(crate) fn automation_name(value: &serde_json::Value, key: &str) -> Result<String> {
    let name = value
        .as_str()
        .with_context(|| format!("{key} must be a string"))?;
    anyhow::ensure!(!name.trim().is_empty(), "{key} must not be empty");
    anyhow::ensure!(name.len() <= 4_096, "{key} exceeds 4096 bytes");
    Ok(name.to_owned())
}

pub(crate) fn automation_optional_name(
    value: &serde_json::Value,
    key: &str,
) -> Result<Option<String>> {
    if value.is_null() {
        Ok(None)
    } else {
        automation_name(value, key).map(Some)
    }
}

pub(crate) fn parse_automation_sam_tag(tag: &str) -> Result<[u8; 2]> {
    let bytes = tag.as_bytes();
    anyhow::ensure!(
        bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_alphanumeric(),
        "tag must be a valid two-character SAM tag"
    );
    Ok([bytes[0], bytes[1]])
}
