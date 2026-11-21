use std::{
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use rgv_core::{
    InteractionRecordingState, InteractionRecordingStatus, STRESS_HEALTH_SCHEMA_VERSION,
    StressHealthSnapshot, StressInteractionState,
};

const NO_INTERACTION: u64 = 0;

/// Lock-free health publication shared with the local-control worker.
///
/// The UI owns the mutable application state, but it only copies bounded counters into this
/// structure. The control thread can therefore sample it even when the UI event loop is blocked.
pub(crate) struct StressHealthPublisher {
    application_instance_id: String,
    process_id: u32,
    started: Instant,
    ui_heartbeat_sequence: AtomicU64,
    ui_last_elapsed_millis: AtomicU64,
    native_input_sequence: AtomicU64,
    frame_sequence: AtomicU64,
    desired_generation: AtomicU64,
    committed_generation: AtomicU64,
    worker_progress_sequence: AtomicU64,
    worker_last_elapsed_millis: AtomicU64,
    next_interaction_id: AtomicU64,
    active_interaction_id: AtomicU64,
    interaction_state: AtomicU8,
    terminal_failure_code: AtomicU8,
    recording_state: AtomicU8,
    recording_input_events: AtomicU64,
    recording_last_sequence: AtomicU64,
    recording_last_durable_sequence: AtomicU64,
    recording_coalesced_events: AtomicU64,
    recording_rejected_events: AtomicU64,
}

impl StressHealthPublisher {
    pub(crate) fn new(application_instance_id: String) -> Self {
        Self {
            application_instance_id,
            process_id: std::process::id(),
            started: Instant::now(),
            ui_heartbeat_sequence: AtomicU64::new(0),
            ui_last_elapsed_millis: AtomicU64::new(0),
            native_input_sequence: AtomicU64::new(0),
            frame_sequence: AtomicU64::new(0),
            desired_generation: AtomicU64::new(0),
            committed_generation: AtomicU64::new(0),
            worker_progress_sequence: AtomicU64::new(0),
            worker_last_elapsed_millis: AtomicU64::new(0),
            next_interaction_id: AtomicU64::new(0),
            active_interaction_id: AtomicU64::new(NO_INTERACTION),
            interaction_state: AtomicU8::new(interaction_state_code(StressInteractionState::Idle)),
            terminal_failure_code: AtomicU8::new(0),
            recording_state: AtomicU8::new(recording_state_code(
                InteractionRecordingState::Inactive,
            )),
            recording_input_events: AtomicU64::new(0),
            recording_last_sequence: AtomicU64::new(0),
            recording_last_durable_sequence: AtomicU64::new(0),
            recording_coalesced_events: AtomicU64::new(0),
            recording_rejected_events: AtomicU64::new(0),
        }
    }

    pub(crate) fn note_ui_heartbeat(&self) {
        self.ui_heartbeat_sequence.fetch_add(1, Ordering::Relaxed);
        self.ui_last_elapsed_millis
            .store(self.elapsed_millis(), Ordering::Release);
    }

    pub(crate) fn note_native_input(&self) -> u64 {
        self.native_input_sequence.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn note_frame_presented(&self) {
        self.frame_sequence.fetch_add(1, Ordering::Release);
    }

    pub(crate) fn observe_generations(&self, desired: u64, committed: u64) {
        let desired_changed = self.desired_generation.swap(desired, Ordering::AcqRel) != desired;
        let committed_changed =
            self.committed_generation.swap(committed, Ordering::AcqRel) != committed;
        if desired_changed || committed_changed {
            self.note_worker_progress();
        }
    }

    pub(crate) fn note_worker_progress(&self) {
        self.worker_progress_sequence
            .fetch_add(1, Ordering::Relaxed);
        self.worker_last_elapsed_millis
            .store(self.elapsed_millis(), Ordering::Release);
    }

    pub(crate) fn begin_pointer_interaction(&self) -> u64 {
        self.begin_interaction(StressInteractionState::PointerHeld)
    }

    pub(crate) fn begin_scroll_interaction(&self) -> u64 {
        let active = self.active_interaction_id.load(Ordering::Acquire);
        if active != NO_INTERACTION
            && decode_interaction_state(self.interaction_state.load(Ordering::Acquire))
                == StressInteractionState::ScrollActive
        {
            return active;
        }
        self.begin_interaction(StressInteractionState::ScrollActive)
    }

    pub(crate) fn finish_interaction(&self) {
        self.interaction_state.store(
            interaction_state_code(StressInteractionState::Idle),
            Ordering::Release,
        );
        self.active_interaction_id
            .store(NO_INTERACTION, Ordering::Release);
    }

    pub(crate) fn cancel_interaction(&self) {
        self.interaction_state.store(
            interaction_state_code(StressInteractionState::Cancelled),
            Ordering::Release,
        );
        self.active_interaction_id
            .store(NO_INTERACTION, Ordering::Release);
    }

    pub(crate) fn clear_cancelled_interaction(&self) {
        if decode_interaction_state(self.interaction_state.load(Ordering::Acquire))
            == StressInteractionState::Cancelled
        {
            self.interaction_state.store(
                interaction_state_code(StressInteractionState::Idle),
                Ordering::Release,
            );
        }
    }

    pub(crate) fn set_terminal_failure(&self, code: Option<&str>) {
        let code = match code {
            None => 0,
            Some("critical_invariant") => 1,
            Some("render_failed") => 2,
            Some("source_failed") => 3,
            Some(_) => 255,
        };
        self.terminal_failure_code.store(code, Ordering::Release);
    }

    pub(crate) fn publish_recording(&self, status: &InteractionRecordingStatus) {
        self.recording_input_events
            .store(status.input_events, Ordering::Relaxed);
        self.recording_last_sequence
            .store(status.last_sequence, Ordering::Relaxed);
        self.recording_last_durable_sequence
            .store(status.last_durable_sequence, Ordering::Relaxed);
        self.recording_coalesced_events
            .store(status.coalesced_events, Ordering::Relaxed);
        self.recording_rejected_events
            .store(status.rejected_events, Ordering::Relaxed);
        self.recording_state
            .store(recording_state_code(status.state), Ordering::Release);
    }

    pub(crate) fn snapshot(&self) -> StressHealthSnapshot {
        let active_interaction_id = self.active_interaction_id.load(Ordering::Acquire);
        StressHealthSnapshot {
            schema_version: STRESS_HEALTH_SCHEMA_VERSION,
            application_instance_id: self.application_instance_id.clone(),
            process_id: self.process_id,
            sampled_unix_millis: unix_millis(),
            ui_heartbeat_sequence: self.ui_heartbeat_sequence.load(Ordering::Acquire),
            ui_last_elapsed_millis: self.ui_last_elapsed_millis.load(Ordering::Acquire),
            native_input_sequence: self.native_input_sequence.load(Ordering::Acquire),
            frame_sequence: self.frame_sequence.load(Ordering::Acquire),
            desired_generation: self.desired_generation.load(Ordering::Acquire),
            committed_generation: self.committed_generation.load(Ordering::Acquire),
            worker_progress_sequence: self.worker_progress_sequence.load(Ordering::Acquire),
            worker_last_elapsed_millis: self.worker_last_elapsed_millis.load(Ordering::Acquire),
            active_interaction_id: (active_interaction_id != NO_INTERACTION)
                .then_some(active_interaction_id),
            interaction_state: decode_interaction_state(
                self.interaction_state.load(Ordering::Acquire),
            ),
            terminal_failure_code: decode_terminal_failure(
                self.terminal_failure_code.load(Ordering::Acquire),
            ),
            recording: InteractionRecordingStatus {
                state: decode_recording_state(self.recording_state.load(Ordering::Acquire)),
                input_events: self.recording_input_events.load(Ordering::Acquire),
                last_sequence: self.recording_last_sequence.load(Ordering::Acquire),
                last_durable_sequence: self.recording_last_durable_sequence.load(Ordering::Acquire),
                coalesced_events: self.recording_coalesced_events.load(Ordering::Acquire),
                rejected_events: self.recording_rejected_events.load(Ordering::Acquire),
                ..InteractionRecordingStatus::default()
            },
        }
    }

    fn begin_interaction(&self, state: StressInteractionState) -> u64 {
        let id = self.next_interaction_id.fetch_add(1, Ordering::AcqRel) + 1;
        self.active_interaction_id.store(id, Ordering::Release);
        self.interaction_state
            .store(interaction_state_code(state), Ordering::Release);
        id
    }

    fn elapsed_millis(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

const fn interaction_state_code(state: StressInteractionState) -> u8 {
    match state {
        StressInteractionState::Idle => 0,
        StressInteractionState::PointerHeld => 1,
        StressInteractionState::ScrollActive => 2,
        StressInteractionState::Cancelled => 3,
    }
}

const fn decode_interaction_state(code: u8) -> StressInteractionState {
    match code {
        1 => StressInteractionState::PointerHeld,
        2 => StressInteractionState::ScrollActive,
        3 => StressInteractionState::Cancelled,
        _ => StressInteractionState::Idle,
    }
}

const fn recording_state_code(state: InteractionRecordingState) -> u8 {
    match state {
        InteractionRecordingState::Inactive => 0,
        InteractionRecordingState::Recording => 1,
        InteractionRecordingState::Complete => 2,
        InteractionRecordingState::Incomplete => 3,
        InteractionRecordingState::Failed => 4,
    }
}

const fn decode_recording_state(code: u8) -> InteractionRecordingState {
    match code {
        1 => InteractionRecordingState::Recording,
        2 => InteractionRecordingState::Complete,
        3 => InteractionRecordingState::Incomplete,
        4 => InteractionRecordingState::Failed,
        _ => InteractionRecordingState::Inactive,
    }
}

fn decode_terminal_failure(code: u8) -> Option<String> {
    match code {
        0 => None,
        1 => Some("critical_invariant".to_owned()),
        2 => Some("render_failed".to_owned()),
        3 => Some("source_failed".to_owned()),
        _ => Some("other_failure".to_owned()),
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_track_progress_without_application_locks() {
        let health = StressHealthPublisher::new("instance-1".to_owned());
        health.note_ui_heartbeat();
        health.note_native_input();
        health.begin_pointer_interaction();
        health.observe_generations(9, 7);
        health.note_frame_presented();

        let held = health.snapshot();
        assert_eq!(held.application_instance_id, "instance-1");
        assert_eq!(held.ui_heartbeat_sequence, 1);
        assert_eq!(held.native_input_sequence, 1);
        assert_eq!(held.frame_sequence, 1);
        assert_eq!(held.desired_generation, 9);
        assert_eq!(held.committed_generation, 7);
        assert_eq!(held.interaction_state, StressInteractionState::PointerHeld);
        assert!(held.active_interaction_id.is_some());

        health.finish_interaction();
        let released = health.snapshot();
        assert_eq!(released.interaction_state, StressInteractionState::Idle);
        assert!(released.active_interaction_id.is_none());
    }

    #[test]
    fn recording_publication_excludes_mutable_paths_and_identifiers() {
        let health = StressHealthPublisher::new("instance-1".to_owned());
        health.publish_recording(&InteractionRecordingStatus {
            state: InteractionRecordingState::Recording,
            recording_id: Some("private-id".to_owned()),
            path: Some("/private/path".into()),
            input_events: 3,
            last_sequence: 4,
            last_durable_sequence: 2,
            coalesced_events: 1,
            rejected_events: 0,
            failure: None,
        });
        let status = health.snapshot().recording;
        assert_eq!(status.state, InteractionRecordingState::Recording);
        assert_eq!(status.input_events, 3);
        assert_eq!(status.last_durable_sequence, 2);
        assert!(status.path.is_none());
        assert!(status.recording_id.is_none());
    }
}
