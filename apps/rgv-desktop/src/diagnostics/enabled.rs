//! The `Diagnostics` facade backed by the real recorders (`diagnostics` feature on).

use super::interaction_recording::InteractionRecorder;
use super::interaction_trace::InteractionTraceRecorder;
use super::manual_diagnostic_instrumentation::ManualDiagnosticTraceRuntime;
use super::manual_diagnostic_trace::ManualDiagnosticTraceRecorder;
use super::stress_health::StressHealthPublisher;
use super::{HealthSignal, LatencyEvent};
use rgv_core::{
    AutomationInteractionTrace, AutomationWorkspaceSnapshot, InteractionRecordingStatus,
    NativeObservationSummary, StressHealthSnapshot, StressTarget,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Shareable handle to the stress-health publisher. The control service keeps a clone so that
/// `/v1/stress/health` can answer while the UI thread is busy.
#[derive(Clone)]
pub(crate) struct HealthHandle(pub(super) Arc<StressHealthPublisher>);

impl HealthHandle {
    pub(crate) fn new(application_instance_id: String) -> Self {
        Self(Arc::new(StressHealthPublisher::new(
            application_instance_id,
        )))
    }

    pub(crate) fn signal(&self, signal: HealthSignal) {
        match signal {
            HealthSignal::UiHeartbeat => self.0.note_ui_heartbeat(),
            HealthSignal::NativeInput => {
                self.0.note_native_input();
            }
            HealthSignal::FramePresented => self.0.note_frame_presented(),
            HealthSignal::WorkerProgress => self.0.note_worker_progress(),
            HealthSignal::PointerInteractionBegan => {
                self.0.begin_pointer_interaction();
            }
            HealthSignal::ScrollInteractionBegan => {
                self.0.begin_scroll_interaction();
            }
            HealthSignal::InteractionFinished => self.0.finish_interaction(),
            HealthSignal::InteractionCancelled => self.0.cancel_interaction(),
            HealthSignal::CancelledInteractionCleared => self.0.clear_cancelled_interaction(),
            HealthSignal::Generations { desired, committed } => {
                self.0.observe_generations(desired, committed);
            }
        }
    }

    pub(crate) fn snapshot(&self) -> StressHealthSnapshot {
        self.0.snapshot()
    }
}

/// All diagnostic recorders owned by one viewer.
pub(crate) struct Diagnostics {
    pub(super) recording: InteractionRecorder,
    pub(super) latency: InteractionTraceRecorder,
    pub(super) manual: ManualDiagnosticTraceRecorder,
    pub(super) manual_runtime: ManualDiagnosticTraceRuntime,
    pub(super) health: HealthHandle,
}

impl Diagnostics {
    pub(crate) fn new(
        health: HealthHandle,
        trace_output_directory: PathBuf,
        gpu_adapter: String,
        gpu_backend: String,
    ) -> Self {
        Self {
            recording: InteractionRecorder::default(),
            latency: InteractionTraceRecorder::default(),
            manual: ManualDiagnosticTraceRecorder::new(),
            manual_runtime: ManualDiagnosticTraceRuntime::new(
                trace_output_directory,
                gpu_adapter,
                gpu_backend,
            ),
            health,
        }
    }

    /// Publishes a liveness signal to the stress-health snapshot.
    pub(crate) fn health(&self, signal: HealthSignal) {
        self.health.signal(signal);
    }

    /// Whether a pointer-to-present latency trace is collecting. Call sites use this to skip
    /// taking timestamps.
    pub(crate) fn latency_active(&self) -> bool {
        self.latency.is_active()
    }

    /// Feeds the pointer-to-present latency trace.
    pub(crate) fn latency(&mut self, event: LatencyEvent) {
        match event {
            LatencyEvent::Begin => self.latency.begin(),
            LatencyEvent::Release => self.latency.release(),
            LatencyEvent::Stage(stage) => self.latency.record(stage),
            LatencyEvent::StageOnce(stage) => self.latency.record_once(stage),
            LatencyEvent::Phase(stage, duration) => self.latency.record_phase(stage, duration),
            LatencyEvent::ScrollDisplay {
                duration,
                rectangles,
                texts,
                capacity_grew,
            } => self
                .latency
                .record_scroll_display(duration, rectangles, texts, capacity_grew),
            LatencyEvent::RendererPrepare {
                duration,
                primitive_count,
                uploaded_bytes,
                buffer_reallocated,
                cpu_capacity_grew,
                slab_cache_hit,
            } => self.latency.record_renderer_prepare(
                duration,
                primitive_count,
                uploaded_bytes,
                buffer_reallocated,
                cpu_capacity_grew,
                slab_cache_hit,
            ),
            LatencyEvent::FrameFinished(duration) => self.latency.frame_finished(duration),
        }
    }

    pub(crate) fn latency_snapshot(&self) -> Option<AutomationInteractionTrace> {
        self.latency.snapshot()
    }

    /// Opens a native-input observation scope in the active interaction recording.
    pub(crate) fn begin_native_observation(
        &mut self,
        run_id: String,
        step_id: String,
        expected_target: Option<StressTarget>,
    ) -> Result<NativeObservationSummary, String> {
        self.recording
            .begin_observation(run_id, step_id, expected_target)
    }

    pub(crate) fn end_native_observation(&mut self) -> Result<NativeObservationSummary, String> {
        self.recording.end_observation()
    }

    /// Writes a labelled workspace checkpoint into the active interaction recording and
    /// publishes the resulting recording status to stress health.
    pub(crate) fn checkpoint_recording(
        &mut self,
        label: String,
        workspace: AutomationWorkspaceSnapshot,
    ) -> Result<InteractionRecordingStatus, String> {
        let status = self.recording.checkpoint(label, workspace)?;
        self.health.0.publish_recording(&status);
        Ok(status)
    }
}
