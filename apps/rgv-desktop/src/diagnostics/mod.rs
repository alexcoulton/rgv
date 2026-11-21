//! Diagnostics facade.
//!
//! Interaction recording, the pointer-to-present latency trace, the manual diagnostic trace and
//! the stress-health publisher are reached only through [`Diagnostics`] (one field on `Viewer`)
//! and the `Viewer` hooks defined in this module tree. Core paths import nothing else from here
//! except the small event/phase vocabularies below.
//!
//! The implementations are compiled only with the `diagnostics` cargo feature (on by default).
//! Without it `disabled.rs` supplies the same surface as no-ops: nothing is recorded, recording
//! and trace commands report that diagnostics are unavailable, and `/v1/stress/health` serves an
//! empty snapshot. Runtime gating with the feature on is unchanged: every hook still returns
//! early unless its recorder is active.

use std::time::Duration;

#[cfg(feature = "diagnostics")]
mod enabled;
#[cfg(feature = "diagnostics")]
mod interaction_recording;
#[cfg(feature = "diagnostics")]
mod interaction_trace;
#[cfg(feature = "diagnostics")]
mod manual_diagnostic_instrumentation;
#[cfg(feature = "diagnostics")]
mod manual_diagnostic_trace;
#[cfg(feature = "diagnostics")]
mod stress_health;

#[cfg(not(feature = "diagnostics"))]
mod disabled;

#[cfg(not(feature = "diagnostics"))]
pub(crate) use disabled::{Diagnostics, HealthHandle};
#[cfg(feature = "diagnostics")]
pub(crate) use enabled::{Diagnostics, HealthHandle};

/// Structured fields attached to one manual diagnostic trace event.
pub(crate) type ManualDiagnosticTraceFields = serde_json::Map<String, serde_json::Value>;

/// Converts a JSON object into trace fields; any other JSON value yields no fields.
pub(crate) fn object_fields(value: serde_json::Value) -> ManualDiagnosticTraceFields {
    value.as_object().cloned().unwrap_or_default()
}

/// A liveness signal for the stress-health publisher.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(crate) enum HealthSignal {
    UiHeartbeat,
    NativeInput,
    FramePresented,
    WorkerProgress,
    PointerInteractionBegan,
    ScrollInteractionBegan,
    InteractionFinished,
    InteractionCancelled,
    CancelledInteractionCleared,
    /// Desired and committed view generations, as observed by the query scheduler.
    Generations {
        desired: u64,
        committed: u64,
    },
}

/// One observation for the pointer-to-present latency trace.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(crate) enum LatencyEvent {
    /// A gesture started; resets the trace.
    Begin,
    /// The gesture's pointer was released.
    Release,
    Stage(InteractionTraceStage),
    /// Records the stage only the first time it is seen in the current gesture.
    StageOnce(InteractionTraceStage),
    Phase(InteractionTraceStage, Duration),
    ScrollDisplay {
        duration: Duration,
        rectangles: usize,
        texts: usize,
        capacity_grew: bool,
    },
    RendererPrepare {
        duration: Duration,
        primitive_count: usize,
        uploaded_bytes: usize,
        buffer_reallocated: bool,
        cpu_capacity_grew: bool,
        slab_cache_hit: bool,
    },
    FrameFinished(Duration),
}

/// A stage of the pointer-to-present latency trace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
pub(crate) enum InteractionTraceStage {
    PointerDown,
    EguiPointerDown,
    DensityIntentDetected,
    PointerCaptureAcquired,
    DensityChanged,
    PointerReleased,
    ScrollDisplayStarted,
    ScrollDisplayFinished,
    RendererPrepareStarted,
    RendererPrepareFinished,
    EguiTessellationFinished,
    SurfaceAcquired,
    EguiBuffersFinished,
    QueueSubmitted,
    PresentCalled,
    FrameFinished,
}

#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
impl InteractionTraceStage {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::PointerDown => "pointer_down",
            Self::EguiPointerDown => "egui_pointer_down",
            Self::DensityIntentDetected => "density_intent_detected",
            Self::PointerCaptureAcquired => "pointer_capture_acquired",
            Self::DensityChanged => "density_changed",
            Self::PointerReleased => "pointer_released",
            Self::ScrollDisplayStarted => "scroll_display_started",
            Self::ScrollDisplayFinished => "scroll_display_finished",
            Self::RendererPrepareStarted => "renderer_prepare_started",
            Self::RendererPrepareFinished => "renderer_prepare_finished",
            Self::EguiTessellationFinished => "egui_tessellation_finished",
            Self::SurfaceAcquired => "surface_acquired",
            Self::EguiBuffersFinished => "egui_buffers_finished",
            Self::QueueSubmitted => "queue_submitted",
            Self::PresentCalled => "present_called",
            Self::FrameFinished => "frame_finished",
        }
    }

    pub(super) const fn bit(self) -> u16 {
        1_u16 << self as u16
    }
}

/// A timed phase of one frame, attributed in the manual diagnostic trace.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ManualDiagnosticFramePhase {
    PollBackground,
    ApplyCapturedMotion,
    FirstDisplayRebuild,
    Shell,
    ShellPreparation,
    ShellDiagnostics,
    ShellInspectors,
    ShellPairSummary,
    ShellScrollbars,
    ShellTextCopy,
    ShellOverviewProjection,
    ShellUiRun,
    ShellPostProcess,
    SecondDisplayRebuild,
    Tessellation,
    SurfaceAcquisition,
    EguiBuffers,
    QueueSubmission,
    Present,
    Prefetch,
}

#[cfg_attr(not(feature = "diagnostics"), allow(dead_code))]
impl ManualDiagnosticFramePhase {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::PollBackground => "poll_background",
            Self::ApplyCapturedMotion => "apply_captured_motion",
            Self::FirstDisplayRebuild => "first_display_rebuild",
            Self::Shell => "shell",
            Self::ShellPreparation => "shell_preparation",
            Self::ShellDiagnostics => "shell_diagnostics",
            Self::ShellInspectors => "shell_inspectors",
            Self::ShellPairSummary => "shell_pair_summary",
            Self::ShellScrollbars => "shell_scrollbars",
            Self::ShellTextCopy => "shell_text_copy",
            Self::ShellOverviewProjection => "shell_overview_projection",
            Self::ShellUiRun => "shell_ui_run",
            Self::ShellPostProcess => "shell_post_process",
            Self::SecondDisplayRebuild => "second_display_rebuild",
            Self::Tessellation => "tessellation",
            Self::SurfaceAcquisition => "surface_acquisition",
            Self::EguiBuffers => "egui_buffers",
            Self::QueueSubmission => "queue_submission",
            Self::Present => "present",
            Self::Prefetch => "prefetch",
        }
    }
}
