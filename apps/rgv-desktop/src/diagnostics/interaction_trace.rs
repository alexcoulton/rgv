use std::time::{Duration, Instant};

use super::InteractionTraceStage;
use rgv_automation::{AutomationInteractionTrace, AutomationInteractionTraceEvent};

const MAXIMUM_TRACE_EVENTS: usize = 128;
const MAXIMUM_TRACE_DURATION: Duration = Duration::from_millis(500);

#[derive(Clone, Copy)]
struct InteractionTraceEvent {
    at: Instant,
    stage: InteractionTraceStage,
    duration: Option<Duration>,
    primitive_count: Option<usize>,
    secondary_count: Option<usize>,
    uploaded_bytes: Option<usize>,
    buffer_reallocated: Option<bool>,
    cpu_capacity_grew: Option<bool>,
    detail: Option<&'static str>,
}

impl InteractionTraceEvent {
    fn simple(stage: InteractionTraceStage) -> Self {
        Self {
            at: Instant::now(),
            stage,
            duration: None,
            primitive_count: None,
            secondary_count: None,
            uploaded_bytes: None,
            buffer_reallocated: None,
            cpu_capacity_grew: None,
            detail: None,
        }
    }
}

pub(crate) struct InteractionTraceRecorder {
    gesture_id: u64,
    started_at: Option<Instant>,
    events: [Option<InteractionTraceEvent>; MAXIMUM_TRACE_EVENTS],
    event_count: usize,
    events_dropped: u64,
    active: bool,
    complete: bool,
    release_seen: bool,
    seen_stages: u16,
}

impl Default for InteractionTraceRecorder {
    fn default() -> Self {
        Self {
            gesture_id: 0,
            started_at: None,
            events: [None; MAXIMUM_TRACE_EVENTS],
            event_count: 0,
            events_dropped: 0,
            active: false,
            complete: false,
            release_seen: false,
            seen_stages: 0,
        }
    }
}

impl InteractionTraceRecorder {
    pub(crate) fn begin(&mut self) {
        self.gesture_id = self.gesture_id.saturating_add(1);
        let now = Instant::now();
        self.started_at = Some(now);
        self.events = [None; MAXIMUM_TRACE_EVENTS];
        self.event_count = 0;
        self.events_dropped = 0;
        self.active = true;
        self.complete = false;
        self.release_seen = false;
        self.seen_stages = InteractionTraceStage::PointerDown.bit();
        self.push(InteractionTraceEvent {
            at: now,
            ..InteractionTraceEvent::simple(InteractionTraceStage::PointerDown)
        });
    }

    pub(crate) fn record(&mut self, stage: InteractionTraceStage) {
        self.push(InteractionTraceEvent::simple(stage));
    }

    pub(crate) fn record_once(&mut self, stage: InteractionTraceStage) {
        let bit = stage.bit();
        if self.seen_stages & bit != 0 {
            return;
        }
        self.seen_stages |= bit;
        self.record(stage);
    }

    pub(crate) fn record_scroll_display(
        &mut self,
        duration: Duration,
        rectangles: usize,
        texts: usize,
        capacity_grew: bool,
    ) {
        self.push(InteractionTraceEvent {
            duration: Some(duration),
            primitive_count: Some(rectangles),
            secondary_count: Some(texts),
            buffer_reallocated: None,
            cpu_capacity_grew: Some(capacity_grew),
            ..InteractionTraceEvent::simple(InteractionTraceStage::ScrollDisplayFinished)
        });
    }

    pub(crate) fn record_renderer_prepare(
        &mut self,
        duration: Duration,
        primitive_count: usize,
        uploaded_bytes: usize,
        buffer_reallocated: bool,
        cpu_capacity_grew: bool,
        slab_cache_hit: bool,
    ) {
        self.push(InteractionTraceEvent {
            duration: Some(duration),
            primitive_count: Some(primitive_count),
            uploaded_bytes: Some(uploaded_bytes),
            buffer_reallocated: Some(buffer_reallocated),
            cpu_capacity_grew: Some(cpu_capacity_grew),
            detail: if slab_cache_hit {
                Some("gpu_slab_hit")
            } else {
                cpu_capacity_grew.then_some("cpu_instance_capacity_grew")
            },
            ..InteractionTraceEvent::simple(InteractionTraceStage::RendererPrepareFinished)
        });
    }

    pub(crate) fn record_phase(&mut self, stage: InteractionTraceStage, duration: Duration) {
        self.push(InteractionTraceEvent {
            duration: Some(duration),
            ..InteractionTraceEvent::simple(stage)
        });
    }

    pub(crate) fn release(&mut self) {
        if !self.active {
            return;
        }
        self.release_seen = true;
        self.record(InteractionTraceStage::PointerReleased);
    }

    pub(crate) fn frame_finished(&mut self, duration: Duration) {
        if !self.active {
            return;
        }
        self.record_phase(InteractionTraceStage::FrameFinished, duration);
        if self.release_seen {
            self.active = false;
            self.complete = true;
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn snapshot(&self) -> Option<AutomationInteractionTrace> {
        let started_at = self.started_at?;
        Some(AutomationInteractionTrace {
            gesture_id: self.gesture_id,
            active: self.active,
            complete: self.complete,
            events: self.events[..self.event_count]
                .iter()
                .flatten()
                .enumerate()
                .map(|(index, event)| AutomationInteractionTraceEvent {
                    index,
                    elapsed_ms: event.at.duration_since(started_at).as_secs_f64() * 1_000.0,
                    stage: event.stage.label().to_owned(),
                    duration_ms: event.duration.map(|value| value.as_secs_f64() * 1_000.0),
                    primitive_count: event.primitive_count,
                    secondary_count: event.secondary_count,
                    uploaded_bytes: event.uploaded_bytes,
                    buffer_reallocated: event.buffer_reallocated,
                    cpu_capacity_grew: event.cpu_capacity_grew,
                    detail: event.detail.map(str::to_owned),
                })
                .collect(),
            events_dropped: self.events_dropped,
        })
    }

    fn push(&mut self, event: InteractionTraceEvent) {
        if !self.active {
            return;
        }
        let Some(started_at) = self.started_at else {
            return;
        };
        if event.at.duration_since(started_at) > MAXIMUM_TRACE_DURATION {
            self.active = false;
            self.complete = true;
            return;
        }
        if self.event_count == MAXIMUM_TRACE_EVENTS {
            self.events_dropped = self.events_dropped.saturating_add(1);
            return;
        }
        self.events[self.event_count] = Some(event);
        self.event_count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_is_bounded_and_snapshot_is_chronological() {
        let mut recorder = InteractionTraceRecorder::default();
        recorder.begin();
        recorder.record_once(InteractionTraceStage::EguiPointerDown);
        recorder.record_once(InteractionTraceStage::EguiPointerDown);
        recorder.release();
        recorder.frame_finished(Duration::from_millis(8));

        let snapshot = recorder.snapshot().unwrap();
        assert!(snapshot.complete);
        assert!(!snapshot.active);
        assert_eq!(snapshot.events[0].stage, "pointer_down");
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| event.stage == "egui_pointer_down")
                .count(),
            1
        );
        assert!(
            snapshot
                .events
                .windows(2)
                .all(|events| { events[0].elapsed_ms <= events[1].elapsed_ms })
        );
    }
}
