use crate::Viewer;
use rgv_automation::{AutomationRectSnapshot, AutomationWorkspaceSnapshot};
use rgv_core::PresentationNodeId;
use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{SyncSender, TrySendError, sync_channel},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use winit::keyboard::ModifiersState;

use rgv_automation::{
    INTERACTION_RECORDING_SCHEMA_NAME, INTERACTION_RECORDING_SCHEMA_VERSION, InteractionLifecycle,
    InteractionRecordingCheckpoint, InteractionRecordingEntry, InteractionRecordingFooter,
    InteractionRecordingHeader, InteractionRecordingState, InteractionRecordingStatus,
    NativeInputObservation, NativeObservationSummary, RecordedReadScroll, StressModifiers,
    StressNativeEvent, StressPoint, StressScrollUnit, StressTarget, StressTouchPhase,
};
use rgv_core::TrackChannel;
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent},
    keyboard::{KeyCode, PhysicalKey},
};

const RECORDING_QUEUE_CAPACITY: usize = 16_384;
const RECORDING_START_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct InteractionRecorder {
    active: Option<ActiveRecording>,
    last_status: InteractionRecordingStatus,
}

struct ActiveRecording {
    recording_id: String,
    output_path: PathBuf,
    partial_path: PathBuf,
    started: Instant,
    sender: SyncSender<WriterMessage>,
    worker: Option<JoinHandle<Result<bool, String>>>,
    durable_sequence: Arc<AtomicU64>,
    writer_failed: Arc<AtomicBool>,
    writer_failure: Arc<Mutex<Option<String>>>,
    sequence: u64,
    input_events: u64,
    coalesced_events: u64,
    rejected_events: u64,
    record_text: bool,
    held_pointer_button: Option<HeldPointerButton>,
    next_interaction_id: u64,
    current_interaction_id: Option<u64>,
    observation: Option<ActiveObservation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeldPointerButton {
    Primary,
    Secondary,
}

#[derive(Clone)]
struct ActiveObservation {
    summary: NativeObservationSummary,
    interaction_id: Option<u64>,
    expected_target: Option<StressTarget>,
    last_motion_position: Option<[f64; 2]>,
    motion_direction: [i8; 2],
    scroll_direction: [i8; 2],
}

enum WriterMessage {
    Entry {
        sequence: u64,
        entry: InteractionRecordingEntry,
    },
    Finish {
        sequence: u64,
        entry: InteractionRecordingEntry,
        finalise: bool,
    },
}

pub(crate) struct PendingNativeInputObservation {
    sequence: u64,
    elapsed_micros: u64,
    event: StressNativeEvent,
    target: Option<StressTarget>,
    target_rect: Option<AutomationRectSnapshot>,
    physical_position: Option<[f64; 2]>,
    logical_position: Option<[f64; 2]>,
    layout_revision: u64,
    egui_consumed: bool,
    accepted_by_handler: bool,
    lifecycle: InteractionLifecycle,
    interaction_id: Option<u64>,
    before_interval: Option<[u64; 2]>,
    before_read_scroll: Option<RecordedReadScroll>,
}

impl Default for InteractionRecorder {
    fn default() -> Self {
        Self {
            active: None,
            last_status: InteractionRecordingStatus::default(),
        }
    }
}

impl InteractionRecorder {
    pub(crate) fn start(
        &mut self,
        output_path: PathBuf,
        header: InteractionRecordingHeader,
    ) -> Result<InteractionRecordingStatus, String> {
        if self.active.is_some() {
            return Err("an interaction recording is already active".to_owned());
        }
        if output_path.exists() {
            return Err(format!(
                "interaction recording output already exists: {}",
                output_path.display()
            ));
        }
        let partial_path = partial_recording_path(&output_path);
        if partial_path.exists() {
            return Err(format!(
                "partial interaction recording already exists: {}",
                partial_path.display()
            ));
        }
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!("could not create the interaction recording directory: {error}")
            })?;
        }

        let recording_id = header.recording_id.clone();
        let record_text = header.record_text;
        let (sender, receiver) = sync_channel(RECORDING_QUEUE_CAPACITY);
        let (ready_sender, ready_receiver) = sync_channel(1);
        let durable_sequence = Arc::new(AtomicU64::new(0));
        let writer_failed = Arc::new(AtomicBool::new(false));
        let writer_failure = Arc::new(Mutex::new(None));
        let worker_durable = Arc::clone(&durable_sequence);
        let worker_failed = Arc::clone(&writer_failed);
        let worker_failure = Arc::clone(&writer_failure);
        let worker_partial = partial_path.clone();
        let worker_output = output_path.clone();
        let worker = std::thread::Builder::new()
            .name("rgv-interaction-recording".to_owned())
            .spawn(move || {
                recording_writer_loop(
                    &worker_partial,
                    &worker_output,
                    header,
                    receiver,
                    ready_sender,
                    &worker_durable,
                    &worker_failed,
                    &worker_failure,
                )
            })
            .map_err(|error| {
                format!("could not start the interaction recording writer: {error}")
            })?;

        match ready_receiver.recv_timeout(RECORDING_START_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(error) => {
                drop(sender);
                let _ = worker.join();
                return Err(format!(
                    "interaction recording writer did not become ready: {error}"
                ));
            }
        }

        self.active = Some(ActiveRecording {
            recording_id,
            output_path,
            partial_path,
            started: Instant::now(),
            sender,
            worker: Some(worker),
            durable_sequence,
            writer_failed,
            writer_failure,
            sequence: 0,
            input_events: 0,
            coalesced_events: 0,
            rejected_events: 0,
            record_text,
            held_pointer_button: None,
            next_interaction_id: 0,
            current_interaction_id: None,
            observation: None,
        });
        self.last_status = self.status();
        Ok(self.last_status.clone())
    }

    pub(crate) fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) fn record_text(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|recording| recording.record_text)
    }

    fn held_pointer_button(&self) -> Option<HeldPointerButton> {
        self.active
            .as_ref()
            .and_then(|recording| recording.held_pointer_button)
    }

    fn interaction_active(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|recording| recording.current_interaction_id.is_some())
    }

    pub(crate) fn elapsed_micros(&self) -> Option<u64> {
        self.active.as_ref().map(|recording| {
            u64::try_from(recording.started.elapsed().as_micros()).unwrap_or(u64::MAX)
        })
    }

    pub(crate) fn next_sequence(&mut self) -> Option<u64> {
        let recording = self.active.as_mut()?;
        recording.sequence = recording.sequence.saturating_add(1);
        Some(recording.sequence)
    }

    fn begin_interaction(&mut self, held_pointer_button: Option<HeldPointerButton>) -> Option<u64> {
        let recording = self.active.as_mut()?;
        recording.next_interaction_id = recording.next_interaction_id.saturating_add(1);
        recording.current_interaction_id = Some(recording.next_interaction_id);
        recording.held_pointer_button = held_pointer_button;
        recording.current_interaction_id
    }

    pub(crate) fn current_interaction_id(&self) -> Option<u64> {
        self.active
            .as_ref()
            .and_then(|recording| recording.current_interaction_id)
    }

    pub(crate) fn release_interaction(&mut self) {
        if let Some(recording) = self.active.as_mut() {
            recording.held_pointer_button = None;
            recording.current_interaction_id = None;
        }
    }

    pub(crate) fn active_step_id(&self) -> Option<String> {
        self.active
            .as_ref()
            .and_then(|recording| recording.observation.as_ref())
            .map(|observation| observation.summary.step_id.clone())
    }

    pub(crate) fn begin_observation(
        &mut self,
        run_id: String,
        step_id: String,
        expected_target: Option<StressTarget>,
    ) -> Result<NativeObservationSummary, String> {
        let recording = self.active.as_mut().ok_or_else(|| {
            "native observation requires an active interaction recording".to_owned()
        })?;
        if recording.observation.is_some() {
            return Err("a native observation scope is already active".to_owned());
        }
        let summary = NativeObservationSummary {
            run_id,
            step_id,
            started_sequence: recording.sequence.saturating_add(1),
            ended_sequence: recording.sequence,
            ..NativeObservationSummary::default()
        };
        recording.observation = Some(ActiveObservation {
            summary: summary.clone(),
            interaction_id: None,
            expected_target,
            last_motion_position: None,
            motion_direction: [0, 0],
            scroll_direction: [0, 0],
        });
        Ok(summary)
    }

    pub(crate) fn end_observation(&mut self) -> Result<NativeObservationSummary, String> {
        let recording = self.active.as_mut().ok_or_else(|| {
            "native observation requires an active interaction recording".to_owned()
        })?;
        let mut observation = recording
            .observation
            .take()
            .ok_or_else(|| "no native observation scope is active".to_owned())?;
        observation.summary.ended_sequence = recording.sequence;
        Ok(observation.summary)
    }

    pub(crate) fn append_input(&mut self, observation: NativeInputObservation) {
        let Some(recording) = self.active.as_mut() else {
            return;
        };
        recording.input_events = recording.input_events.saturating_add(1);
        if let Some(scope) = recording.observation.as_mut() {
            if let Some(interaction_id) = observation.interaction_id
                && observation.lifecycle != InteractionLifecycle::None
            {
                match scope.interaction_id {
                    None => scope.interaction_id = Some(interaction_id),
                    Some(expected) if expected != interaction_id => {
                        scope.summary.extraneous_events =
                            scope.summary.extraneous_events.saturating_add(1);
                    }
                    Some(_) => {}
                }
            }
            scope.summary.received_events = scope.summary.received_events.saturating_add(1);
            if observation.viewer_accepted {
                scope.summary.applied_events = scope.summary.applied_events.saturating_add(1);
                scope.summary.target_acquired |= observation_target_matches(
                    scope.expected_target.as_ref(),
                    observation.target.as_ref(),
                );
                scope.summary.began |= observation.lifecycle == InteractionLifecycle::Began;
                scope.summary.advanced |= observation.lifecycle == InteractionLifecycle::Advanced;
                scope.summary.terminated |= matches!(
                    observation.lifecycle,
                    InteractionLifecycle::Released | InteractionLifecycle::Cancelled
                );
            }
            observe_event_shape(scope, &observation);
        }
        let sequence = observation.sequence;
        enqueue_entry(
            recording,
            sequence,
            InteractionRecordingEntry::Input(observation),
        );
    }

    pub(crate) fn checkpoint(
        &mut self,
        label: String,
        workspace: AutomationWorkspaceSnapshot,
    ) -> Result<InteractionRecordingStatus, String> {
        let sequence = self
            .next_sequence()
            .ok_or_else(|| "no interaction recording is active".to_owned())?;
        let elapsed_micros = self.elapsed_micros().unwrap_or_default();
        let recording = self.active.as_mut().expect("recording checked above");
        enqueue_entry(
            recording,
            sequence,
            InteractionRecordingEntry::Checkpoint(InteractionRecordingCheckpoint {
                sequence,
                elapsed_micros,
                label,
                workspace,
            }),
        );
        Ok(self.status())
    }

    pub(crate) fn stop(&mut self) -> Result<InteractionRecordingStatus, String> {
        self.finish(true, None)
    }

    pub(crate) fn abort(&mut self, reason: &str) -> Result<InteractionRecordingStatus, String> {
        self.finish(false, Some(reason.to_owned()))
    }

    pub(crate) fn status(&self) -> InteractionRecordingStatus {
        let Some(recording) = self.active.as_ref() else {
            return self.last_status.clone();
        };
        let failure = writer_failure_message(&recording.writer_failed, &recording.writer_failure);
        InteractionRecordingStatus {
            state: if failure.is_some() {
                InteractionRecordingState::Failed
            } else {
                InteractionRecordingState::Recording
            },
            recording_id: Some(recording.recording_id.clone()),
            path: Some(recording.partial_path.clone()),
            input_events: recording.input_events,
            last_sequence: recording.sequence,
            last_durable_sequence: recording.durable_sequence.load(Ordering::Acquire),
            coalesced_events: recording.coalesced_events,
            rejected_events: recording.rejected_events,
            failure,
        }
    }

    fn finish(
        &mut self,
        requested_complete: bool,
        reason: Option<String>,
    ) -> Result<InteractionRecordingStatus, String> {
        let Some(mut recording) = self.active.take() else {
            return Err("no interaction recording is active".to_owned());
        };
        if recording.observation.is_some() && requested_complete {
            self.active = Some(recording);
            return Err("cannot stop while a native observation scope is active".to_owned());
        }
        recording.sequence = recording.sequence.saturating_add(1);
        let writer_failure =
            writer_failure_message(&recording.writer_failed, &recording.writer_failure);
        let complete = requested_complete
            && writer_failure.is_none()
            && recording.rejected_events == 0
            && recording.current_interaction_id.is_none();
        let footer_reason = reason.or_else(|| {
            (!complete).then(|| {
                writer_failure.clone().unwrap_or_else(|| {
                    if recording.current_interaction_id.is_some() {
                        "recording stopped while native input was active".to_owned()
                    } else {
                        "recording evidence was incomplete".to_owned()
                    }
                })
            })
        });
        let footer = InteractionRecordingEntry::Footer(InteractionRecordingFooter {
            sequence: recording.sequence,
            elapsed_micros: u64::try_from(recording.started.elapsed().as_micros())
                .unwrap_or(u64::MAX),
            complete,
            input_events: recording.input_events,
            coalesced_events: recording.coalesced_events,
            rejected_events: recording.rejected_events,
            reason: footer_reason.clone(),
        });
        let finish_sent = recording.sender.try_send(WriterMessage::Finish {
            sequence: recording.sequence,
            entry: footer,
            finalise: complete,
        });
        if let Err(error) = finish_sent {
            recording.rejected_events = recording.rejected_events.saturating_add(1);
            set_writer_failure(
                &recording.writer_failed,
                &recording.writer_failure,
                format!("could not enqueue recording footer: {error}"),
            );
        }
        drop(recording.sender);
        let worker_result = recording.worker.take().map_or_else(
            || Ok(false),
            |worker| {
                worker
                    .join()
                    .map_err(|_| "interaction recording writer panicked".to_owned())?
            },
        );
        let finalised = worker_result.as_ref().copied().unwrap_or(false);
        let worker_error = worker_result.err().or_else(|| {
            writer_failure_message(&recording.writer_failed, &recording.writer_failure)
        });
        let state = if complete && finalised && worker_error.is_none() {
            InteractionRecordingState::Complete
        } else if worker_error.is_some() {
            InteractionRecordingState::Failed
        } else {
            InteractionRecordingState::Incomplete
        };
        let failure = worker_error.or(footer_reason);
        self.last_status = InteractionRecordingStatus {
            state,
            recording_id: Some(recording.recording_id),
            path: Some(if finalised {
                recording.output_path
            } else {
                recording.partial_path
            }),
            input_events: recording.input_events,
            last_sequence: recording.sequence,
            last_durable_sequence: recording.durable_sequence.load(Ordering::Acquire),
            coalesced_events: recording.coalesced_events,
            rejected_events: recording.rejected_events,
            failure,
        };
        Ok(self.last_status.clone())
    }
}

fn observation_target_matches(
    expected: Option<&StressTarget>,
    observed: Option<&StressTarget>,
) -> bool {
    match expected {
        None | Some(StressTarget::Window) => observed.is_some(),
        Some(StressTarget::GenomicCanvas { panel_id }) => matches!(
            observed,
            Some(
                StressTarget::GenomicCanvas {
                    panel_id: observed_panel,
                }
                | StressTarget::ReadCanvas {
                    panel_id: observed_panel,
                    ..
                }
                | StressTarget::PaneContent {
                    panel_id: observed_panel,
                    ..
                }
            ) if observed_panel == panel_id
        ),
        Some(StressTarget::TrackHeader { panel_id, target }) => matches!(
            observed,
            Some(
                StressTarget::TrackHeader {
                    panel_id: observed_panel,
                    target: observed_target,
                }
                | StressTarget::PaneHeader {
                    panel_id: observed_panel,
                    target: observed_target,
                }
            ) if observed_panel == panel_id && observed_target == target
        ),
        Some(expected) => observed == Some(expected),
    }
}

fn observe_event_shape(scope: &mut ActiveObservation, observation: &NativeInputObservation) {
    match &observation.event {
        StressNativeEvent::Move { .. } => {
            scope.summary.event_shape.move_events =
                scope.summary.event_shape.move_events.saturating_add(1);
            observe_motion_direction(scope, observation.physical_position);
        }
        StressNativeEvent::PrimaryDown { .. } => {
            scope.summary.event_shape.primary_down_events = scope
                .summary
                .event_shape
                .primary_down_events
                .saturating_add(1);
        }
        StressNativeEvent::PrimaryDrag { .. } => {
            scope.summary.event_shape.primary_drag_events = scope
                .summary
                .event_shape
                .primary_drag_events
                .saturating_add(1);
            observe_motion_direction(scope, observation.physical_position);
        }
        StressNativeEvent::PrimaryUp { .. } => {
            scope.summary.event_shape.primary_up_events = scope
                .summary
                .event_shape
                .primary_up_events
                .saturating_add(1);
        }
        StressNativeEvent::SecondaryDown { .. } => {
            scope.summary.event_shape.secondary_down_events = scope
                .summary
                .event_shape
                .secondary_down_events
                .saturating_add(1);
        }
        StressNativeEvent::SecondaryDrag { .. } => {
            scope.summary.event_shape.secondary_drag_events = scope
                .summary
                .event_shape
                .secondary_drag_events
                .saturating_add(1);
            observe_motion_direction(scope, observation.physical_position);
        }
        StressNativeEvent::SecondaryUp { .. } => {
            scope.summary.event_shape.secondary_up_events = scope
                .summary
                .event_shape
                .secondary_up_events
                .saturating_add(1);
        }
        StressNativeEvent::Scroll {
            delta_x,
            delta_y,
            phase,
            ..
        } => {
            let counter = match phase {
                StressTouchPhase::None => &mut scope.summary.event_shape.scroll_none_events,
                StressTouchPhase::Began => &mut scope.summary.event_shape.scroll_began_events,
                StressTouchPhase::Changed => &mut scope.summary.event_shape.scroll_changed_events,
                StressTouchPhase::Ended => &mut scope.summary.event_shape.scroll_ended_events,
                StressTouchPhase::Cancelled => {
                    &mut scope.summary.event_shape.scroll_cancelled_events
                }
                StressTouchPhase::MayBegin => {
                    &mut scope.summary.event_shape.scroll_may_begin_events
                }
            };
            *counter = counter.saturating_add(1);
            scope.summary.event_shape.direction_reversals = scope
                .summary
                .event_shape
                .direction_reversals
                .saturating_add(observe_axis_direction(
                    &mut scope.scroll_direction[0],
                    *delta_x,
                ))
                .saturating_add(observe_axis_direction(
                    &mut scope.scroll_direction[1],
                    *delta_y,
                ));
        }
        StressNativeEvent::KeyDown { .. } => {
            scope.summary.event_shape.key_down_events =
                scope.summary.event_shape.key_down_events.saturating_add(1);
        }
        StressNativeEvent::KeyUp { .. } => {
            scope.summary.event_shape.key_up_events =
                scope.summary.event_shape.key_up_events.saturating_add(1);
        }
        StressNativeEvent::Focus { .. } => {
            scope.summary.event_shape.focus_events =
                scope.summary.event_shape.focus_events.saturating_add(1);
        }
        StressNativeEvent::ResizeWindow { .. } => {
            scope.summary.event_shape.resize_events =
                scope.summary.event_shape.resize_events.saturating_add(1);
        }
        StressNativeEvent::Unsupported { .. } => {}
    }
}

fn observe_motion_direction(scope: &mut ActiveObservation, position: Option<[f64; 2]>) {
    let Some(position) = position else {
        return;
    };
    if let Some(previous) = scope.last_motion_position {
        scope.summary.event_shape.direction_reversals = scope
            .summary
            .event_shape
            .direction_reversals
            .saturating_add(observe_axis_direction(
                &mut scope.motion_direction[0],
                position[0] - previous[0],
            ))
            .saturating_add(observe_axis_direction(
                &mut scope.motion_direction[1],
                position[1] - previous[1],
            ));
    }
    scope.last_motion_position = Some(position);
}

fn observe_axis_direction(previous: &mut i8, delta: f64) -> u64 {
    let direction = if delta > f64::EPSILON {
        1
    } else if delta < -f64::EPSILON {
        -1
    } else {
        return 0;
    };
    let reversed = *previous != 0 && *previous != direction;
    *previous = direction;
    u64::from(reversed)
}

fn enqueue_entry(recording: &mut ActiveRecording, sequence: u64, entry: InteractionRecordingEntry) {
    match recording
        .sender
        .try_send(WriterMessage::Entry { sequence, entry })
    {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            recording.rejected_events = recording.rejected_events.saturating_add(1);
            set_writer_failure(
                &recording.writer_failed,
                &recording.writer_failure,
                "interaction recording queue filled; structural evidence was rejected".to_owned(),
            );
        }
        Err(TrySendError::Disconnected(_)) => {
            recording.rejected_events = recording.rejected_events.saturating_add(1);
            set_writer_failure(
                &recording.writer_failed,
                &recording.writer_failure,
                "interaction recording writer disconnected".to_owned(),
            );
        }
    }
}

fn recording_writer_loop(
    partial_path: &Path,
    output_path: &Path,
    header: InteractionRecordingHeader,
    receiver: std::sync::mpsc::Receiver<WriterMessage>,
    ready: SyncSender<Result<(), String>>,
    durable_sequence: &AtomicU64,
    failed: &AtomicBool,
    failure: &Mutex<Option<String>>,
) -> Result<bool, String> {
    let result: Result<bool, String> = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(partial_path)
            .map_err(|error| format!("could not create interaction recording: {error}"))?;
        let mut writer = BufWriter::new(file);
        write_recording_entry(&mut writer, &InteractionRecordingEntry::Header(header))?;
        writer
            .flush()
            .map_err(|error| format!("could not flush interaction recording header: {error}"))?;
        ready
            .send(Ok(()))
            .map_err(|_| "interaction recording startup acknowledgement was dropped".to_owned())?;

        while let Ok(message) = receiver.recv() {
            match message {
                WriterMessage::Entry { sequence, entry } => {
                    write_recording_entry(&mut writer, &entry)?;
                    writer.flush().map_err(|error| {
                        format!("could not flush interaction recording entry: {error}")
                    })?;
                    durable_sequence.store(sequence, Ordering::Release);
                }
                WriterMessage::Finish {
                    sequence,
                    entry,
                    finalise,
                } => {
                    write_recording_entry(&mut writer, &entry)?;
                    writer.flush().map_err(|error| {
                        format!("could not flush interaction recording footer: {error}")
                    })?;
                    writer.get_ref().sync_all().map_err(|error| {
                        format!("could not sync interaction recording: {error}")
                    })?;
                    durable_sequence.store(sequence, Ordering::Release);
                    drop(writer);
                    if finalise {
                        std::fs::rename(partial_path, output_path).map_err(|error| {
                            format!("could not finalise interaction recording: {error}")
                        })?;
                    }
                    return Ok(finalise);
                }
            }
        }
        Ok(false)
    })();
    if let Err(error) = &result {
        let _ = ready.try_send(Err(error.clone()));
        set_writer_failure(failed, failure, error.clone());
    }
    result
}

fn write_recording_entry(
    writer: &mut BufWriter<File>,
    entry: &InteractionRecordingEntry,
) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, entry)
        .map_err(|error| format!("could not serialize interaction recording entry: {error}"))?;
    writer
        .write_all(b"\n")
        .map_err(|error| format!("could not write interaction recording entry: {error}"))
}

fn set_writer_failure(failed: &AtomicBool, failure: &Mutex<Option<String>>, error: String) {
    // The flag is authoritative and cannot be lost if the diagnostic mutex is contended.
    failed.store(true, Ordering::Release);
    if let Ok(mut failure) = failure.try_lock()
        && failure.is_none()
    {
        *failure = Some(error);
    }
}

fn writer_failure_message(failed: &AtomicBool, failure: &Mutex<Option<String>>) -> Option<String> {
    if !failed.load(Ordering::Acquire) {
        return None;
    }
    failure
        .try_lock()
        .ok()
        .and_then(|failure| failure.clone())
        .or_else(|| Some("interaction recording writer reported evidence loss".to_owned()))
}

fn partial_recording_path(output_path: &Path) -> PathBuf {
    let file_name = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("recording.jsonl");
    let partial_name = file_name.strip_suffix(".jsonl").map_or_else(
        || format!("{file_name}.partial"),
        |stem| format!("{stem}.partial.jsonl"),
    );
    output_path.with_file_name(partial_name)
}

impl Viewer {
    pub(crate) fn start_interaction_recording(
        &mut self,
        path: PathBuf,
        fixture: Option<String>,
        record_text: bool,
    ) -> Result<InteractionRecordingStatus, String> {
        let header = InteractionRecordingHeader {
            schema: INTERACTION_RECORDING_SCHEMA_NAME.to_owned(),
            schema_version: INTERACTION_RECORDING_SCHEMA_VERSION,
            recording_id: uuid::Uuid::new_v4().to_string(),
            application_instance_id: self.application_instance_id.clone(),
            process_id: std::process::id(),
            application_version: env!("CARGO_PKG_VERSION").to_owned(),
            started_unix_millis: unix_millis(),
            surface_size_pixels: [self.surface_config.width, self.surface_config.height],
            scale_factor: self.window.scale_factor(),
            fixture,
            record_text,
            initial_workspace: Some(self.automation_workspace_snapshot()),
        };
        let status = self.diagnostics.recording.start(path, header)?;
        self.diagnostics.health.0.publish_recording(&status);
        Ok(status)
    }

    pub(crate) fn stop_interaction_recording(
        &mut self,
    ) -> Result<InteractionRecordingStatus, String> {
        let status = self.diagnostics.recording.stop()?;
        self.diagnostics.health.0.publish_recording(&status);
        Ok(status)
    }

    pub(crate) fn abort_interaction_recording_before_exit(&mut self) {
        if self.diagnostics.recording.is_recording() {
            match self
                .diagnostics
                .recording
                .abort("RGV exited while interaction recording was active")
            {
                Ok(status) => self.diagnostics.health.0.publish_recording(&status),
                Err(error) => tracing::warn!(%error, "interaction recording could not be aborted"),
            }
        }
    }

    pub(crate) fn interaction_recording_status(&self) -> InteractionRecordingStatus {
        self.diagnostics.recording.status()
    }

    pub(crate) fn prepare_native_input_observation(
        &mut self,
        event: &WindowEvent,
        egui_consumed: bool,
    ) -> Option<PendingNativeInputObservation> {
        if !self.diagnostics.recording.is_recording() {
            return None;
        }
        let scale = self.window.scale_factor();
        let physical_position = match event {
            WindowEvent::CursorMoved { position, .. } => Some([position.x, position.y]),
            _ => Some([self.cursor.x, self.cursor.y]),
        };
        let logical_position = physical_position.map(|[x, y]| [x / scale, y / scale]);
        let logical_point = logical_position.map(|[x, y]| egui::pos2(x as f32, y as f32));
        let (mut target, mut target_rect) = logical_point
            .map(|point| self.recording_target_at(point))
            .unwrap_or((None, None));
        let target_position = logical_point
            .zip(target_rect)
            .map_or(StressPoint::default(), |(point, rect)| {
                normalized_point(point, rect)
            });
        let elapsed_micros = self.diagnostics.recording.elapsed_micros()?;
        let modifiers = stress_modifiers(self.modifiers);
        let held_pointer_button = self.diagnostics.recording.held_pointer_button();
        let interaction_active = self.diagnostics.recording.interaction_active();
        let record_text = self.diagnostics.recording.record_text();
        let event = recording_event(
            event,
            elapsed_micros,
            target_position,
            modifiers,
            held_pointer_button,
            record_text,
            scale,
        )?;
        if matches!(
            event,
            StressNativeEvent::Focus { .. } | StressNativeEvent::ResizeWindow { .. }
        ) {
            let logical = self.window.inner_size().to_logical::<f64>(scale);
            target = Some(StressTarget::Window);
            target_rect = Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(logical.width as f32, logical.height as f32),
            ));
        }
        let lifecycle = recording_lifecycle(&event, interaction_active);
        let interaction_id = match lifecycle {
            InteractionLifecycle::Began => {
                let held_pointer_button = match &event {
                    StressNativeEvent::PrimaryDown { .. } => Some(HeldPointerButton::Primary),
                    StressNativeEvent::SecondaryDown { .. } => Some(HeldPointerButton::Secondary),
                    _ => None,
                };
                self.diagnostics
                    .recording
                    .begin_interaction(held_pointer_button)
            }
            InteractionLifecycle::Advanced => self
                .diagnostics
                .recording
                .current_interaction_id()
                .or_else(|| self.diagnostics.recording.begin_interaction(None)),
            InteractionLifecycle::Released | InteractionLifecycle::Cancelled => {
                self.diagnostics.recording.current_interaction_id()
            }
            InteractionLifecycle::None => None,
        };
        let sequence = self.diagnostics.recording.next_sequence()?;
        let (before_interval, before_read_scroll) = self.recording_semantic_state(target.as_ref());
        Some(PendingNativeInputObservation {
            sequence,
            elapsed_micros,
            event,
            target,
            target_rect: target_rect.map(|rect| AutomationRectSnapshot {
                left: rect.left(),
                top: rect.top(),
                right: rect.right(),
                bottom: rect.bottom(),
            }),
            physical_position,
            logical_position,
            layout_revision: self.workspace_layout.revision,
            egui_consumed,
            accepted_by_handler: egui_consumed,
            lifecycle,
            interaction_id,
            before_interval,
            before_read_scroll,
        })
    }

    pub(crate) fn mark_native_input_handled(pending: &mut Option<PendingNativeInputObservation>) {
        if let Some(pending) = pending {
            pending.accepted_by_handler = true;
        }
    }

    pub(crate) fn finish_native_input_observation(
        &mut self,
        pending: Option<PendingNativeInputObservation>,
    ) {
        let Some(pending) = pending else {
            return;
        };
        let (after_interval, after_read_scroll) =
            self.recording_semantic_state(pending.target.as_ref());
        let semantic_changed = pending.before_interval != after_interval
            || pending.before_read_scroll != after_read_scroll;
        let viewer_accepted = pending.accepted_by_handler || semantic_changed;
        let active_panel = self.panels.active();
        let interval = active_panel.viewport.interval();
        let observation = NativeInputObservation {
            sequence: pending.sequence,
            elapsed_micros: pending.elapsed_micros,
            step_id: self.diagnostics.recording.active_step_id(),
            event: pending.event,
            physical_position: pending.physical_position,
            logical_position: pending.logical_position,
            target: pending.target,
            target_rect: pending.target_rect,
            layout_revision: pending.layout_revision,
            egui_consumed: pending.egui_consumed,
            viewer_accepted,
            interaction_id: pending.interaction_id,
            lifecycle: pending.lifecycle,
            active_panel_id: Some(active_panel.id),
            panel_interval: Some([interval.start(), interval.end()]),
            read_scroll: after_read_scroll,
        };
        let release = matches!(
            observation.lifecycle,
            InteractionLifecycle::Released | InteractionLifecycle::Cancelled
        );
        self.diagnostics.recording.append_input(observation);
        if release {
            self.diagnostics.recording.release_interaction();
        }
        let status = self.diagnostics.recording.status();
        self.diagnostics.health.0.publish_recording(&status);
    }

    fn recording_target_at(&self, point: egui::Pos2) -> (Option<StressTarget>, Option<egui::Rect>) {
        for panel_layout in &self.workspace_layout.panels {
            let panel_id = panel_layout.panel_id;
            if panel_layout.context_rows.overview.contains(point) {
                return (
                    Some(StressTarget::Overview { panel_id }),
                    Some(panel_layout.context_rows.overview),
                );
            }
            if panel_layout.context_rows.ruler.contains(point) {
                return (
                    Some(StressTarget::Ruler { panel_id }),
                    Some(panel_layout.context_rows.ruler),
                );
            }
            let Some(panel) = self.panels.iter().find(|panel| panel.id == panel_id) else {
                continue;
            };
            for layout in [&panel.pane_layout.reads, &panel.pane_layout.annotation] {
                for item in &layout.items {
                    if item.header_rect.contains(point) {
                        return (
                            Some(StressTarget::PaneHeader {
                                panel_id,
                                target: item.id,
                            }),
                            Some(item.header_rect),
                        );
                    }
                    if item.content_rect.is_some_and(|rect| rect.contains(point)) {
                        let target = match item.id {
                            PresentationNodeId::Track {
                                channel: TrackChannel::Reads,
                                track_id,
                            } => StressTarget::ReadCanvas { panel_id, track_id },
                            target => StressTarget::PaneContent { panel_id, target },
                        };
                        return (Some(target), item.content_rect);
                    }
                    if panel_layout.scrollbar.contains(point)
                        && item.row_rect.top() <= point.y
                        && point.y <= item.row_rect.bottom()
                        && let PresentationNodeId::Track {
                            channel: TrackChannel::Reads,
                            track_id,
                        } = item.id
                    {
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(panel_layout.scrollbar.left(), item.row_rect.top()),
                            egui::pos2(panel_layout.scrollbar.right(), item.row_rect.bottom()),
                        )
                        .intersect(panel_layout.scrollbar);
                        return (
                            Some(StressTarget::ReadScrollbar { panel_id, track_id }),
                            Some(rect),
                        );
                    }
                }
            }
            if panel_layout.canvas.contains(point) {
                return (
                    Some(StressTarget::GenomicCanvas { panel_id }),
                    Some(panel_layout.canvas),
                );
            }
        }
        let logical = self
            .window
            .inner_size()
            .to_logical::<f64>(self.window.scale_factor());
        let rect = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(logical.width as f32, logical.height as f32),
        );
        (
            Some(StressTarget::WindowCoordinate {
                required_logical_size: [
                    logical.width.round().max(1.0) as u32,
                    logical.height.round().max(1.0) as u32,
                ],
            }),
            Some(rect),
        )
    }

    fn recording_semantic_state(
        &self,
        target: Option<&StressTarget>,
    ) -> (Option<[u64; 2]>, Option<RecordedReadScroll>) {
        let panel_id = match target {
            Some(
                StressTarget::GenomicCanvas { panel_id }
                | StressTarget::ReadCanvas { panel_id, .. }
                | StressTarget::ReadScrollbar { panel_id, .. }
                | StressTarget::TrackHeader { panel_id, .. }
                | StressTarget::Overview { panel_id }
                | StressTarget::Ruler { panel_id }
                | StressTarget::PaneContent { panel_id, .. }
                | StressTarget::PaneHeader { panel_id, .. },
            ) => Some(*panel_id),
            _ => None,
        };
        let panel = panel_id
            .and_then(|panel_id| self.panels.iter().find(|panel| panel.id == panel_id))
            .unwrap_or_else(|| self.panels.active());
        let interval = panel.viewport.interval();
        let read_scroll = match target {
            Some(
                StressTarget::ReadCanvas { track_id, .. }
                | StressTarget::ReadScrollbar { track_id, .. },
            ) => {
                let state = panel.read_panel_scroll_state(*track_id);
                Some(RecordedReadScroll {
                    panel_id: panel.id,
                    track_id: *track_id,
                    points: state.scroll_points,
                    maximum_points: (state.content_bottom_points
                        - panel.pane_layout.reads.pane_rect.bottom())
                    .max(0.0),
                })
            }
            _ => None,
        };
        (Some([interval.start(), interval.end()]), read_scroll)
    }
}

fn recording_event(
    event: &WindowEvent,
    at_micros: u64,
    position: StressPoint,
    modifiers: StressModifiers,
    held_pointer_button: Option<HeldPointerButton>,
    record_text: bool,
    scale: f64,
) -> Option<StressNativeEvent> {
    match event {
        WindowEvent::CursorMoved { .. }
            if held_pointer_button == Some(HeldPointerButton::Primary) =>
        {
            Some(StressNativeEvent::PrimaryDrag {
                at_micros,
                position,
                modifiers,
            })
        }
        WindowEvent::CursorMoved { .. }
            if held_pointer_button == Some(HeldPointerButton::Secondary) =>
        {
            Some(StressNativeEvent::SecondaryDrag {
                at_micros,
                position,
                modifiers,
            })
        }
        WindowEvent::CursorMoved { .. } => Some(StressNativeEvent::Move {
            at_micros,
            position,
            modifiers,
        }),
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } => Some(StressNativeEvent::PrimaryDown {
            at_micros,
            modifiers,
        }),
        WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Left,
            ..
        } => Some(StressNativeEvent::PrimaryUp {
            at_micros,
            modifiers,
        }),
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } => Some(StressNativeEvent::SecondaryDown {
            at_micros,
            modifiers,
        }),
        WindowEvent::MouseInput {
            state: ElementState::Released,
            button: MouseButton::Right,
            ..
        } => Some(StressNativeEvent::SecondaryUp {
            at_micros,
            modifiers,
        }),
        WindowEvent::MouseInput { button, .. } => Some(StressNativeEvent::Unsupported {
            at_micros,
            class: format!("mouse_button_{button:?}").to_ascii_lowercase(),
            reason: "only primary and secondary mouse buttons are replayable".to_owned(),
        }),
        WindowEvent::MouseWheel { delta, phase, .. } => {
            let (delta_x, delta_y, unit) = match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    (f64::from(*x), f64::from(*y), StressScrollUnit::Line)
                }
                MouseScrollDelta::PixelDelta(delta) => {
                    (delta.x / scale, delta.y / scale, StressScrollUnit::Pixel)
                }
            };
            Some(StressNativeEvent::Scroll {
                at_micros,
                delta_x,
                delta_y,
                unit,
                phase: stress_touch_phase(*phase),
                momentum_phase: StressTouchPhase::None,
                continuous: matches!(delta, MouseScrollDelta::PixelDelta(_)),
                modifiers,
            })
        }
        WindowEvent::KeyboardInput { event, .. } => {
            if !record_text
                && event.state == ElementState::Pressed
                && event.text.as_ref().is_some_and(|text| !text.is_empty())
                && !modifiers.command
                && !modifiers.control
            {
                return Some(StressNativeEvent::Unsupported {
                    at_micros,
                    class: "redacted_text_input".to_owned(),
                    reason:
                        "text input is not replayable unless explicit text recording is enabled"
                            .to_owned(),
                });
            }
            let Some(key_code) = mac_key_code(event.physical_key) else {
                return Some(StressNativeEvent::Unsupported {
                    at_micros,
                    class: "keyboard".to_owned(),
                    reason: "the physical key has no supported macOS virtual key mapping"
                        .to_owned(),
                });
            };
            match event.state {
                ElementState::Pressed => Some(StressNativeEvent::KeyDown {
                    at_micros,
                    key_code,
                    text: record_text
                        .then(|| event.text.as_ref().map(ToString::to_string))
                        .flatten(),
                    modifiers,
                }),
                ElementState::Released => Some(StressNativeEvent::KeyUp {
                    at_micros,
                    key_code,
                    modifiers,
                }),
            }
        }
        WindowEvent::Focused(focused) => Some(StressNativeEvent::Focus {
            at_micros,
            focused: *focused,
        }),
        WindowEvent::Resized(size) => Some(StressNativeEvent::ResizeWindow {
            at_micros,
            logical_size: [
                (f64::from(size.width) / scale).round().max(1.0) as u32,
                (f64::from(size.height) / scale).round().max(1.0) as u32,
            ],
        }),
        WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
            Some(StressNativeEvent::Unsupported {
                at_micros,
                class: "scale_factor_changed".to_owned(),
                reason: format!(
                    "display scale changed to {scale_factor}; conversion requires an explicit environment prerequisite"
                ),
            })
        }
        WindowEvent::CursorLeft { .. } => Some(StressNativeEvent::Unsupported {
            at_micros,
            class: "cursor_left".to_owned(),
            reason: "cursor exit is evidence but has no standalone replay primitive".to_owned(),
        }),
        _ => None,
    }
}

fn recording_lifecycle(
    event: &StressNativeEvent,
    interaction_active: bool,
) -> InteractionLifecycle {
    match event {
        StressNativeEvent::PrimaryDown { .. } | StressNativeEvent::SecondaryDown { .. } => {
            InteractionLifecycle::Began
        }
        StressNativeEvent::PrimaryDrag { .. } | StressNativeEvent::SecondaryDrag { .. } => {
            InteractionLifecycle::Advanced
        }
        StressNativeEvent::PrimaryUp { .. } | StressNativeEvent::SecondaryUp { .. } => {
            InteractionLifecycle::Released
        }
        StressNativeEvent::Scroll { phase, .. } => match phase {
            StressTouchPhase::Began => InteractionLifecycle::Began,
            StressTouchPhase::Changed => InteractionLifecycle::Advanced,
            StressTouchPhase::Ended => InteractionLifecycle::Released,
            StressTouchPhase::Cancelled => InteractionLifecycle::Cancelled,
            StressTouchPhase::None | StressTouchPhase::MayBegin => InteractionLifecycle::None,
        },
        StressNativeEvent::Focus { focused: false, .. } if interaction_active => {
            InteractionLifecycle::Cancelled
        }
        _ => InteractionLifecycle::None,
    }
}

const fn stress_touch_phase(phase: TouchPhase) -> StressTouchPhase {
    match phase {
        TouchPhase::Started => StressTouchPhase::Began,
        TouchPhase::Moved => StressTouchPhase::Changed,
        TouchPhase::Ended => StressTouchPhase::Ended,
        TouchPhase::Cancelled => StressTouchPhase::Cancelled,
    }
}

fn stress_modifiers(modifiers: ModifiersState) -> StressModifiers {
    StressModifiers {
        shift: modifiers.shift_key(),
        control: modifiers.control_key(),
        option: modifiers.alt_key(),
        command: modifiers.super_key(),
    }
}

fn normalized_point(point: egui::Pos2, rect: egui::Rect) -> StressPoint {
    StressPoint {
        x: if rect.width() > 0.0 {
            f64::from((point.x - rect.left()) / rect.width())
        } else {
            0.0
        },
        y: if rect.height() > 0.0 {
            f64::from((point.y - rect.top()) / rect.height())
        } else {
            0.0
        },
    }
}

#[allow(clippy::too_many_lines)]
const fn mac_key_code(physical: PhysicalKey) -> Option<u16> {
    let PhysicalKey::Code(code) = physical else {
        return None;
    };
    Some(match code {
        KeyCode::KeyA => 0,
        KeyCode::KeyS => 1,
        KeyCode::KeyD => 2,
        KeyCode::KeyF => 3,
        KeyCode::KeyH => 4,
        KeyCode::KeyG => 5,
        KeyCode::KeyZ => 6,
        KeyCode::KeyX => 7,
        KeyCode::KeyC => 8,
        KeyCode::KeyV => 9,
        KeyCode::KeyB => 11,
        KeyCode::KeyQ => 12,
        KeyCode::KeyW => 13,
        KeyCode::KeyE => 14,
        KeyCode::KeyR => 15,
        KeyCode::KeyY => 16,
        KeyCode::KeyT => 17,
        KeyCode::Digit1 => 18,
        KeyCode::Digit2 => 19,
        KeyCode::Digit3 => 20,
        KeyCode::Digit4 => 21,
        KeyCode::Digit6 => 22,
        KeyCode::Digit5 => 23,
        KeyCode::Equal => 24,
        KeyCode::Digit9 => 25,
        KeyCode::Digit7 => 26,
        KeyCode::Minus => 27,
        KeyCode::Digit8 => 28,
        KeyCode::Digit0 => 29,
        KeyCode::BracketRight => 30,
        KeyCode::KeyO => 31,
        KeyCode::KeyU => 32,
        KeyCode::BracketLeft => 33,
        KeyCode::KeyI => 34,
        KeyCode::KeyP => 35,
        KeyCode::Enter => 36,
        KeyCode::KeyL => 37,
        KeyCode::KeyJ => 38,
        KeyCode::Quote => 39,
        KeyCode::KeyK => 40,
        KeyCode::Semicolon => 41,
        KeyCode::Backslash => 42,
        KeyCode::Comma => 43,
        KeyCode::Slash => 44,
        KeyCode::KeyN => 45,
        KeyCode::KeyM => 46,
        KeyCode::Period => 47,
        KeyCode::Tab => 48,
        KeyCode::Space => 49,
        KeyCode::Backquote => 50,
        KeyCode::Backspace => 51,
        KeyCode::Escape => 53,
        KeyCode::SuperLeft | KeyCode::SuperRight => 55,
        KeyCode::ShiftLeft => 56,
        KeyCode::CapsLock => 57,
        KeyCode::AltLeft => 58,
        KeyCode::ControlLeft => 59,
        KeyCode::ShiftRight => 60,
        KeyCode::AltRight => 61,
        KeyCode::ControlRight => 62,
        KeyCode::F17 => 64,
        KeyCode::NumpadDecimal => 65,
        KeyCode::NumpadMultiply => 67,
        KeyCode::NumpadAdd => 69,
        KeyCode::NumLock => 71,
        KeyCode::NumpadDivide => 75,
        KeyCode::NumpadEnter => 76,
        KeyCode::NumpadSubtract => 78,
        KeyCode::F18 => 79,
        KeyCode::F19 => 80,
        KeyCode::NumpadEqual => 81,
        KeyCode::Numpad0 => 82,
        KeyCode::Numpad1 => 83,
        KeyCode::Numpad2 => 84,
        KeyCode::Numpad3 => 85,
        KeyCode::Numpad4 => 86,
        KeyCode::Numpad5 => 87,
        KeyCode::Numpad6 => 88,
        KeyCode::Numpad7 => 89,
        KeyCode::F20 => 90,
        KeyCode::Numpad8 => 91,
        KeyCode::Numpad9 => 92,
        KeyCode::F5 => 96,
        KeyCode::F6 => 97,
        KeyCode::F7 => 98,
        KeyCode::F3 => 99,
        KeyCode::F8 => 100,
        KeyCode::F9 => 101,
        KeyCode::F11 => 103,
        KeyCode::F13 => 105,
        KeyCode::F16 => 106,
        KeyCode::F14 => 107,
        KeyCode::F10 => 109,
        KeyCode::F12 => 111,
        KeyCode::F15 => 113,
        KeyCode::Insert => 114,
        KeyCode::Home => 115,
        KeyCode::PageUp => 116,
        KeyCode::Delete => 117,
        KeyCode::F4 => 118,
        KeyCode::End => 119,
        KeyCode::F2 => 120,
        KeyCode::PageDown => 121,
        KeyCode::F1 => 122,
        KeyCode::ArrowLeft => 123,
        KeyCode::ArrowRight => 124,
        KeyCode::ArrowDown => 125,
        KeyCode::ArrowUp => 126,
        _ => return None,
    })
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
    use rgv_core::PanelId;
    use std::io::{BufRead as _, BufReader};

    use super::*;

    fn header() -> InteractionRecordingHeader {
        InteractionRecordingHeader {
            schema: INTERACTION_RECORDING_SCHEMA_NAME.to_owned(),
            schema_version: INTERACTION_RECORDING_SCHEMA_VERSION,
            recording_id: "recording-1".to_owned(),
            application_instance_id: "instance-1".to_owned(),
            process_id: 7,
            application_version: "test".to_owned(),
            started_unix_millis: 1,
            surface_size_pixels: [800, 600],
            scale_factor: 2.0,
            fixture: Some("fixture".to_owned()),
            record_text: false,
            initial_workspace: None,
        }
    }

    #[test]
    fn clean_stop_atomically_replaces_partial_journal() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("recording.jsonl");
        let partial = partial_recording_path(&output);
        let mut recorder = InteractionRecorder::default();
        recorder.start(output.clone(), header()).unwrap();
        let status = recorder.stop().unwrap();
        assert_eq!(status.state, InteractionRecordingState::Complete);
        assert!(output.is_file());
        assert!(!partial.exists());

        let entries = BufReader::new(File::open(output).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<InteractionRecordingEntry>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            entries.first(),
            Some(InteractionRecordingEntry::Header(_))
        ));
        assert!(matches!(
            entries.last(),
            Some(InteractionRecordingEntry::Footer(
                InteractionRecordingFooter { complete: true, .. }
            ))
        ));
    }

    #[test]
    fn abort_retains_a_readable_partial_journal() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("recording.jsonl");
        let partial = partial_recording_path(&output);
        let mut recorder = InteractionRecorder::default();
        recorder.start(output.clone(), header()).unwrap();
        let status = recorder.abort("test interruption").unwrap();
        assert_eq!(status.state, InteractionRecordingState::Incomplete);
        assert!(!output.exists());
        assert!(partial.is_file());
        assert!(
            std::fs::read_to_string(partial)
                .unwrap()
                .contains("test interruption")
        );
    }

    #[test]
    fn phase_less_scroll_is_not_left_active_and_secondary_motion_advances() {
        let scroll = StressNativeEvent::Scroll {
            at_micros: 0,
            delta_x: 0.0,
            delta_y: 1.0,
            unit: StressScrollUnit::Line,
            phase: StressTouchPhase::None,
            momentum_phase: StressTouchPhase::None,
            continuous: false,
            modifiers: StressModifiers::default(),
        };
        assert_eq!(
            recording_lifecycle(&scroll, false),
            InteractionLifecycle::None
        );
        assert_eq!(
            recording_lifecycle(
                &StressNativeEvent::SecondaryDrag {
                    at_micros: 1,
                    position: StressPoint { x: 0.5, y: 0.5 },
                    modifiers: StressModifiers::default(),
                },
                true,
            ),
            InteractionLifecycle::Advanced
        );
    }

    #[test]
    fn broad_canvas_target_accepts_nested_canvas_content_in_the_same_panel() {
        let panel = PanelId::new(1);
        let expected = StressTarget::GenomicCanvas { panel_id: panel };
        assert!(observation_target_matches(
            Some(&expected),
            Some(&StressTarget::ReadCanvas {
                panel_id: panel,
                track_id: rgv_core::TrackId::new(7),
            })
        ));
        assert!(observation_target_matches(
            Some(&expected),
            Some(&StressTarget::PaneContent {
                panel_id: panel,
                target: rgv_core::PresentationNodeId::Reference,
            })
        ));
        assert!(!observation_target_matches(
            Some(&expected),
            Some(&StressTarget::ReadCanvas {
                panel_id: PanelId::new(2),
                track_id: rgv_core::TrackId::new(7),
            })
        ));
        assert!(!observation_target_matches(
            Some(&expected),
            Some(&StressTarget::ReadScrollbar {
                panel_id: panel,
                track_id: rgv_core::TrackId::new(7),
            })
        ));
    }

    #[test]
    fn legacy_track_header_target_matches_the_recorded_pane_header_name() {
        let panel = PanelId::new(1);
        let target = rgv_core::PresentationNodeId::Reference;
        assert!(observation_target_matches(
            Some(&StressTarget::TrackHeader {
                panel_id: panel,
                target,
            }),
            Some(&StressTarget::PaneHeader {
                panel_id: panel,
                target,
            })
        ));
    }

    #[test]
    fn observation_does_not_claim_target_or_lifecycle_for_unhandled_input() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("recording.jsonl");
        let target = StressTarget::GenomicCanvas {
            panel_id: PanelId::new(1),
        };
        let mut recorder = InteractionRecorder::default();
        recorder.start(output, header()).unwrap();
        recorder
            .begin_observation(
                "run-1".to_owned(),
                "step-1".to_owned(),
                Some(target.clone()),
            )
            .unwrap();
        recorder.append_input(NativeInputObservation {
            sequence: 1,
            elapsed_micros: 0,
            step_id: Some("step-1".to_owned()),
            event: StressNativeEvent::PrimaryDown {
                at_micros: 0,
                modifiers: StressModifiers::default(),
            },
            physical_position: Some([10.0, 10.0]),
            logical_position: Some([10.0, 10.0]),
            target: Some(target),
            target_rect: None,
            layout_revision: 1,
            egui_consumed: false,
            viewer_accepted: false,
            interaction_id: Some(1),
            lifecycle: InteractionLifecycle::Began,
            active_panel_id: Some(PanelId::new(1)),
            panel_interval: Some([0, 100]),
            read_scroll: None,
        });
        let summary = recorder.end_observation().unwrap();
        assert_eq!(summary.received_events, 1);
        assert_eq!(summary.applied_events, 0);
        assert!(!summary.target_acquired);
        assert!(!summary.began);
        recorder.stop().unwrap();
    }
}
