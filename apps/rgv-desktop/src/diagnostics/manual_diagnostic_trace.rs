use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Map, Value};
use uuid::Uuid;

pub(crate) const MANUAL_DIAGNOSTIC_TRACE_SCHEMA_NAME: &str = "rgv.manual_diagnostic_trace";
pub(crate) const MANUAL_DIAGNOSTIC_TRACE_SCHEMA_VERSION: u64 = 3;
pub(crate) const MANUAL_DIAGNOSTIC_TRACE_MAX_EVENTS: usize = 100_000;
pub(crate) const MANUAL_DIAGNOSTIC_TRACE_RETENTION: Duration = Duration::from_secs(300);

const JSON_MEDIA_TYPE: &str = "application/json";

/// Caller-supplied structured diagnostic data.
///
/// The recorder deliberately gathers no paths, user names, host names, read names, or genomic
/// source identifiers on its own. Call sites remain responsible for supplying aggregate or
/// redacted values here.
pub(crate) use super::ManualDiagnosticTraceFields;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ManualDiagnosticTraceState {
    #[default]
    Inactive,
    Recording,
    Frozen,
}

impl ManualDiagnosticTraceState {
    const fn label(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Recording => "recording",
            Self::Frozen => "frozen",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManualDiagnosticTraceSessionId(String);

impl ManualDiagnosticTraceSessionId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ManualDiagnosticInteractionId(u64);

impl ManualDiagnosticInteractionId {
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManualDiagnosticTraceStatus {
    pub(crate) state: ManualDiagnosticTraceState,
    pub(crate) session_id: Option<String>,
    pub(crate) retained_events: usize,
    pub(crate) overwritten_events: u64,
    pub(crate) dropped_events: u64,
    pub(crate) active_interactions: usize,
    pub(crate) first_elapsed_us: Option<u64>,
    pub(crate) last_elapsed_us: Option<u64>,
    pub(crate) frozen_at_event_sequence: Option<u64>,
    pub(crate) max_events: usize,
    pub(crate) retention_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManualDiagnosticTraceExportPayload {
    pub(crate) suggested_filename: String,
    pub(crate) media_type: &'static str,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManualDiagnosticEventKind {
    Event,
    InteractionBegin,
    InteractionEvent,
    InteractionEnd,
    CriticalFailure,
}

impl ManualDiagnosticEventKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::InteractionBegin => "interaction_begin",
            Self::InteractionEvent => "interaction_event",
            Self::InteractionEnd => "interaction_end",
            Self::CriticalFailure => "critical_failure",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ManualDiagnosticTraceEvent {
    sequence: u64,
    elapsed_us: u64,
    category: String,
    name: String,
    kind: ManualDiagnosticEventKind,
    interaction_id: Option<ManualDiagnosticInteractionId>,
    duration_us: Option<u64>,
    fields: ManualDiagnosticTraceFields,
}

impl ManualDiagnosticTraceEvent {
    fn json(&self) -> Value {
        let mut event = Map::new();
        event.insert("sequence".to_owned(), Value::from(self.sequence));
        event.insert("elapsed_us".to_owned(), Value::from(self.elapsed_us));
        event.insert("category".to_owned(), Value::from(self.category.clone()));
        event.insert("name".to_owned(), Value::from(self.name.clone()));
        event.insert("kind".to_owned(), Value::from(self.kind.label()));
        if let Some(interaction_id) = self.interaction_id {
            event.insert(
                "interaction_id".to_owned(),
                Value::from(interaction_id.get()),
            );
        }
        if let Some(duration_us) = self.duration_us {
            event.insert("duration_us".to_owned(), Value::from(duration_us));
        }
        event.insert("fields".to_owned(), Value::Object(self.fields.clone()));
        Value::Object(event)
    }
}

#[derive(Clone, Debug)]
struct ManualDiagnosticTraceSession {
    id: ManualDiagnosticTraceSessionId,
    started_unix_ms: u64,
    started_at: Instant,
    fields: ManualDiagnosticTraceFields,
}

#[derive(Clone, Copy, Debug)]
struct ActiveManualDiagnosticInteraction {
    started_at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct ManualDiagnosticTraceLimits {
    max_events: usize,
    retention: Duration,
}

impl Default for ManualDiagnosticTraceLimits {
    fn default() -> Self {
        Self {
            max_events: MANUAL_DIAGNOSTIC_TRACE_MAX_EVENTS,
            retention: MANUAL_DIAGNOSTIC_TRACE_RETENTION,
        }
    }
}

/// An explicitly started, memory-only diagnostic recorder.
///
/// The event deque is allocated to its complete bounded capacity up front. Recording never opens
/// or writes a file; JSON allocation happens only when an export API is called.
pub(crate) struct ManualDiagnosticTraceRecorder {
    state: ManualDiagnosticTraceState,
    limits: ManualDiagnosticTraceLimits,
    session: Option<ManualDiagnosticTraceSession>,
    events: VecDeque<ManualDiagnosticTraceEvent>,
    active_interactions: BTreeMap<ManualDiagnosticInteractionId, ActiveManualDiagnosticInteraction>,
    next_event_sequence: Option<u64>,
    next_interaction_id: Option<u64>,
    overwritten_events: u64,
    dropped_events: u64,
    frozen_at_event_sequence: Option<u64>,
    last_recorded_elapsed_us: Option<u64>,
}

impl Default for ManualDiagnosticTraceRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl ManualDiagnosticTraceRecorder {
    pub(crate) fn new() -> Self {
        Self::with_limits(ManualDiagnosticTraceLimits::default())
    }

    fn with_limits(limits: ManualDiagnosticTraceLimits) -> Self {
        assert!(limits.max_events > 0, "trace capacity must be non-zero");
        Self {
            state: ManualDiagnosticTraceState::Inactive,
            limits,
            session: None,
            events: VecDeque::with_capacity(limits.max_events),
            active_interactions: BTreeMap::new(),
            next_event_sequence: Some(1),
            next_interaction_id: Some(1),
            overwritten_events: 0,
            dropped_events: 0,
            frozen_at_event_sequence: None,
            last_recorded_elapsed_us: None,
        }
    }

    pub(crate) fn start_with_metadata(
        &mut self,
        fields: ManualDiagnosticTraceFields,
    ) -> ManualDiagnosticTraceSessionId {
        self.start_at(
            ManualDiagnosticTraceSessionId(Uuid::new_v4().to_string()),
            SystemTime::now(),
            Instant::now(),
            fields,
        )
    }

    fn start_at(
        &mut self,
        id: ManualDiagnosticTraceSessionId,
        wall_clock: SystemTime,
        monotonic_clock: Instant,
        fields: ManualDiagnosticTraceFields,
    ) -> ManualDiagnosticTraceSessionId {
        self.events.clear();
        self.active_interactions.clear();
        self.next_event_sequence = Some(1);
        self.next_interaction_id = Some(1);
        self.overwritten_events = 0;
        self.dropped_events = 0;
        self.frozen_at_event_sequence = None;
        self.last_recorded_elapsed_us = None;
        self.state = ManualDiagnosticTraceState::Recording;
        self.session = Some(ManualDiagnosticTraceSession {
            id: id.clone(),
            started_unix_ms: unix_milliseconds(wall_clock),
            started_at: monotonic_clock,
            fields,
        });
        id
    }

    pub(crate) fn begin_interaction(
        &mut self,
        category: impl Into<String>,
        name: impl Into<String>,
        fields: ManualDiagnosticTraceFields,
    ) -> Option<ManualDiagnosticInteractionId> {
        self.begin_interaction_at(Instant::now(), category.into(), name.into(), fields)
    }

    fn begin_interaction_at(
        &mut self,
        now: Instant,
        category: String,
        name: String,
        fields: ManualDiagnosticTraceFields,
    ) -> Option<ManualDiagnosticInteractionId> {
        if !self.accepts_events() {
            self.note_dropped_event();
            return None;
        }
        let Some(interaction_id) = self.allocate_interaction_id() else {
            self.note_dropped_event();
            return None;
        };
        self.append_event_at(
            now,
            category,
            name,
            ManualDiagnosticEventKind::InteractionBegin,
            Some(interaction_id),
            None,
            fields,
        )?;
        self.active_interactions.insert(
            interaction_id,
            ActiveManualDiagnosticInteraction { started_at: now },
        );
        Some(interaction_id)
    }

    pub(crate) fn record_interaction(
        &mut self,
        interaction_id: ManualDiagnosticInteractionId,
        category: impl Into<String>,
        name: impl Into<String>,
        fields: ManualDiagnosticTraceFields,
    ) -> bool {
        self.record_interaction_at(
            Instant::now(),
            interaction_id,
            category.into(),
            name.into(),
            fields,
        )
    }

    fn record_interaction_at(
        &mut self,
        now: Instant,
        interaction_id: ManualDiagnosticInteractionId,
        category: String,
        name: String,
        fields: ManualDiagnosticTraceFields,
    ) -> bool {
        if !self.accepts_events() || !self.active_interactions.contains_key(&interaction_id) {
            self.note_dropped_event();
            return false;
        }
        self.append_event_at(
            now,
            category,
            name,
            ManualDiagnosticEventKind::InteractionEvent,
            Some(interaction_id),
            None,
            fields,
        )
        .is_some()
    }

    pub(crate) fn end_interaction(
        &mut self,
        interaction_id: ManualDiagnosticInteractionId,
        category: impl Into<String>,
        name: impl Into<String>,
        fields: ManualDiagnosticTraceFields,
    ) -> bool {
        self.end_interaction_at(
            Instant::now(),
            interaction_id,
            category.into(),
            name.into(),
            fields,
        )
    }

    fn end_interaction_at(
        &mut self,
        now: Instant,
        interaction_id: ManualDiagnosticInteractionId,
        category: String,
        name: String,
        fields: ManualDiagnosticTraceFields,
    ) -> bool {
        if !self.accepts_events() {
            self.note_dropped_event();
            return false;
        }
        let Some(active) = self.active_interactions.remove(&interaction_id) else {
            self.note_dropped_event();
            return false;
        };
        let duration_us = duration_micros(
            now.checked_duration_since(active.started_at)
                .unwrap_or_default(),
        );
        self.append_event_at(
            now,
            category,
            name,
            ManualDiagnosticEventKind::InteractionEnd,
            Some(interaction_id),
            Some(duration_us),
            fields,
        )
        .is_some()
    }

    pub(crate) fn record(
        &mut self,
        category: impl Into<String>,
        name: impl Into<String>,
        fields: ManualDiagnosticTraceFields,
    ) -> bool {
        self.record_at(Instant::now(), category.into(), name.into(), fields)
    }

    fn record_at(
        &mut self,
        now: Instant,
        category: String,
        name: String,
        fields: ManualDiagnosticTraceFields,
    ) -> bool {
        if !self.accepts_events() {
            self.note_dropped_event();
            return false;
        }
        self.append_event_at(
            now,
            category,
            name,
            ManualDiagnosticEventKind::Event,
            None,
            None,
            fields,
        )
        .is_some()
    }

    /// Records the failure as the final event and prevents all subsequent event mutation.
    ///
    /// Returns `true` only for the `Recording -> Frozen` transition. Calling this while inactive
    /// or after an earlier freeze is a rejected event attempt and returns `false`.
    pub(crate) fn freeze_on_critical_failure(
        &mut self,
        category: impl Into<String>,
        name: impl Into<String>,
        fields: ManualDiagnosticTraceFields,
    ) -> bool {
        self.freeze_on_critical_failure_at(Instant::now(), category.into(), name.into(), fields)
    }

    fn freeze_on_critical_failure_at(
        &mut self,
        now: Instant,
        category: String,
        name: String,
        fields: ManualDiagnosticTraceFields,
    ) -> bool {
        if !self.accepts_events() {
            self.note_dropped_event();
            return false;
        }
        let sequence = self.append_event_at(
            now,
            category,
            name,
            ManualDiagnosticEventKind::CriticalFailure,
            None,
            None,
            fields,
        );
        self.frozen_at_event_sequence = sequence;
        self.state = ManualDiagnosticTraceState::Frozen;
        true
    }

    /// Stops an ordinary recording. A frozen failure trace intentionally stays frozen until a
    /// new session is explicitly started, preventing an incidental stop call from losing that
    /// state signal.
    pub(crate) fn stop(&mut self) -> bool {
        if self.state != ManualDiagnosticTraceState::Recording {
            return false;
        }
        self.active_interactions.clear();
        self.state = ManualDiagnosticTraceState::Inactive;
        true
    }

    pub(crate) fn status(&self) -> ManualDiagnosticTraceStatus {
        ManualDiagnosticTraceStatus {
            state: self.state,
            session_id: self.session.as_ref().map(|session| session.id.0.clone()),
            retained_events: self.events.len(),
            overwritten_events: self.overwritten_events,
            dropped_events: self.dropped_events,
            active_interactions: self.active_interactions.len(),
            first_elapsed_us: self.events.front().map(|event| event.elapsed_us),
            last_elapsed_us: self.events.back().map(|event| event.elapsed_us),
            frozen_at_event_sequence: self.frozen_at_event_sequence,
            max_events: self.limits.max_events,
            retention_seconds: self.limits.retention.as_secs(),
        }
    }

    pub(crate) fn json_document(&self) -> Value {
        let status = self.status();
        let session = self.session.as_ref().map_or(Value::Null, |session| {
            serde_json::json!({
                "id": session.id.as_str(),
                "started_unix_ms": session.started_unix_ms,
                "application_version": env!("CARGO_PKG_VERSION"),
                "metadata": session.fields,
            })
        });
        serde_json::json!({
            "schema": {
                "name": MANUAL_DIAGNOSTIC_TRACE_SCHEMA_NAME,
                "version": MANUAL_DIAGNOSTIC_TRACE_SCHEMA_VERSION,
            },
            "session": session,
            "recorder": {
                "state": status.state.label(),
                "retention": {
                    "max_events": status.max_events,
                    "max_age_seconds": status.retention_seconds,
                },
                "statistics": {
                    "events_retained": status.retained_events,
                    "events_overwritten": status.overwritten_events,
                    "events_dropped": status.dropped_events,
                    "active_interactions": status.active_interactions,
                    "first_elapsed_us": status.first_elapsed_us,
                    "last_elapsed_us": status.last_elapsed_us,
                    "frozen_at_event_sequence": status.frozen_at_event_sequence,
                },
            },
            "events": self.events.iter().map(ManualDiagnosticTraceEvent::json).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn pretty_json_document(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(&self.json_document())
    }

    pub(crate) fn export_payload(&self) -> serde_json::Result<ManualDiagnosticTraceExportPayload> {
        let session_label = self
            .session
            .as_ref()
            .map_or("inactive", |session| session.id.as_str());
        Ok(ManualDiagnosticTraceExportPayload {
            suggested_filename: format!("rgv-manual-diagnostic-trace-{session_label}.json"),
            media_type: JSON_MEDIA_TYPE,
            bytes: self.pretty_json_document()?.into_bytes(),
        })
    }

    fn accepts_events(&self) -> bool {
        self.state == ManualDiagnosticTraceState::Recording && self.session.is_some()
    }

    fn note_dropped_event(&mut self) {
        if self.session.is_some() {
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
    }

    fn allocate_event_sequence(&mut self) -> Option<u64> {
        let sequence = self.next_event_sequence?;
        self.next_event_sequence = sequence.checked_add(1);
        Some(sequence)
    }

    fn allocate_interaction_id(&mut self) -> Option<ManualDiagnosticInteractionId> {
        let id = self.next_interaction_id?;
        self.next_interaction_id = id.checked_add(1);
        Some(ManualDiagnosticInteractionId(id))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_event_at(
        &mut self,
        now: Instant,
        category: String,
        name: String,
        kind: ManualDiagnosticEventKind,
        interaction_id: Option<ManualDiagnosticInteractionId>,
        duration_us: Option<u64>,
        fields: ManualDiagnosticTraceFields,
    ) -> Option<u64> {
        let Some(sequence) = self.allocate_event_sequence() else {
            self.note_dropped_event();
            return None;
        };
        let raw_elapsed_us = self.session.as_ref().map_or(0, |session| {
            duration_micros(
                now.checked_duration_since(session.started_at)
                    .unwrap_or_default(),
            )
        });
        let elapsed_us = self
            .last_recorded_elapsed_us
            .map_or(raw_elapsed_us, |previous| previous.max(raw_elapsed_us));
        self.last_recorded_elapsed_us = Some(elapsed_us);
        self.evict_expired(elapsed_us);
        while self.events.len() >= self.limits.max_events {
            self.events.pop_front();
            self.overwritten_events = self.overwritten_events.saturating_add(1);
        }
        self.events.push_back(ManualDiagnosticTraceEvent {
            sequence,
            elapsed_us,
            category,
            name,
            kind,
            interaction_id,
            duration_us,
            fields,
        });
        Some(sequence)
    }

    fn evict_expired(&mut self, newest_elapsed_us: u64) {
        let retention_us = duration_micros(self.limits.retention);
        while self.events.front().is_some_and(|oldest| {
            newest_elapsed_us.saturating_sub(oldest.elapsed_us) > retention_us
        }) {
            self.events.pop_front();
            self.overwritten_events = self.overwritten_events.saturating_add(1);
        }
    }
}

fn unix_milliseconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorder_with_limits(
        max_events: usize,
        retention: Duration,
    ) -> ManualDiagnosticTraceRecorder {
        ManualDiagnosticTraceRecorder::with_limits(ManualDiagnosticTraceLimits {
            max_events,
            retention,
        })
    }

    fn start_at(recorder: &mut ManualDiagnosticTraceRecorder, origin: Instant) {
        recorder.start_at(
            ManualDiagnosticTraceSessionId("test-session".to_owned()),
            UNIX_EPOCH + Duration::from_secs(123),
            origin,
            ManualDiagnosticTraceFields::new(),
        );
    }

    fn fields(
        entries: impl IntoIterator<Item = (&'static str, Value)>,
    ) -> ManualDiagnosticTraceFields {
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect()
    }

    #[test]
    fn event_count_and_age_bounds_evict_oldest_and_count_overwrites() {
        let origin = Instant::now();
        let mut recorder = recorder_with_limits(3, Duration::from_secs(2));
        start_at(&mut recorder, origin);
        assert!(recorder.record_at(
            origin,
            "render".to_owned(),
            "one".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert!(recorder.record_at(
            origin + Duration::from_secs(1),
            "render".to_owned(),
            "two".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert!(recorder.record_at(
            origin + Duration::from_secs(2),
            "render".to_owned(),
            "three".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert!(recorder.record_at(
            origin + Duration::from_micros(2_000_001),
            "render".to_owned(),
            "four".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        let status = recorder.status();
        assert_eq!(status.retained_events, 3);
        assert_eq!(status.overwritten_events, 1);
        assert_eq!(status.first_elapsed_us, Some(1_000_000));

        assert!(recorder.record_at(
            origin + Duration::from_micros(2_000_002),
            "render".to_owned(),
            "five".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        let status = recorder.status();
        assert_eq!(status.retained_events, 3);
        assert_eq!(status.overwritten_events, 2);
        assert_eq!(recorder.events.front().map(|event| event.sequence), Some(3));
        assert!(recorder.events.capacity() >= 3);
    }

    #[test]
    fn events_remain_chronological_with_monotonic_sequences_and_elapsed_time() {
        let origin = Instant::now();
        let mut recorder = recorder_with_limits(8, Duration::from_secs(120));
        start_at(&mut recorder, origin);
        for (offset, name) in [(4, "first"), (2, "second"), (9, "third")] {
            assert!(recorder.record_at(
                origin + Duration::from_micros(offset),
                "navigation".to_owned(),
                name.to_owned(),
                ManualDiagnosticTraceFields::new(),
            ));
        }
        let events = recorder.events.iter().collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.elapsed_us)
                .collect::<Vec<_>>(),
            [4, 4, 9]
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn interactions_have_monotonic_ids_correlation_and_end_duration() {
        let origin = Instant::now();
        let mut recorder = recorder_with_limits(16, Duration::from_secs(120));
        start_at(&mut recorder, origin);
        let first = recorder
            .begin_interaction_at(
                origin + Duration::from_micros(10),
                "input".to_owned(),
                "pan".to_owned(),
                fields([("device", Value::from("pointer"))]),
            )
            .expect("recording accepts interaction");
        let second = recorder
            .begin_interaction_at(
                origin + Duration::from_micros(12),
                "input".to_owned(),
                "zoom".to_owned(),
                ManualDiagnosticTraceFields::new(),
            )
            .expect("recording accepts interaction");
        assert_eq!(first.get(), 1);
        assert_eq!(second.get(), 2);
        assert!(recorder.record_interaction_at(
            origin + Duration::from_micros(15),
            first,
            "camera".to_owned(),
            "moved".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert!(recorder.end_interaction_at(
            origin + Duration::from_micros(30),
            first,
            "input".to_owned(),
            "pan_complete".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert_eq!(recorder.status().active_interactions, 1);
        let correlated = recorder
            .events
            .iter()
            .filter(|event| event.interaction_id == Some(first))
            .collect::<Vec<_>>();
        assert_eq!(correlated.len(), 3);
        assert_eq!(
            correlated.last().and_then(|event| event.duration_us),
            Some(20)
        );
    }

    #[test]
    fn critical_failure_is_final_event_and_freezes_trace() {
        let origin = Instant::now();
        let mut recorder = recorder_with_limits(8, Duration::from_secs(120));
        start_at(&mut recorder, origin);
        assert!(recorder.record_at(
            origin + Duration::from_micros(1),
            "scene".to_owned(),
            "prepared".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert!(recorder.freeze_on_critical_failure_at(
            origin + Duration::from_micros(2),
            "scene".to_owned(),
            "camera_mismatch".to_owned(),
            fields([("track", Value::from(2))]),
        ));
        let frozen_status = recorder.status();
        assert_eq!(frozen_status.state, ManualDiagnosticTraceState::Frozen);
        assert_eq!(frozen_status.frozen_at_event_sequence, Some(2));
        assert_eq!(
            recorder.events.back().map(|event| event.kind),
            Some(ManualDiagnosticEventKind::CriticalFailure)
        );
        assert!(!recorder.record_at(
            origin + Duration::from_micros(3),
            "scene".to_owned(),
            "too_late".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert!(!recorder.freeze_on_critical_failure_at(
            origin + Duration::from_micros(4),
            "scene".to_owned(),
            "second_failure".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert_eq!(recorder.status().retained_events, 2);
        assert_eq!(recorder.status().dropped_events, 2);
        assert!(!recorder.stop());
        assert_eq!(recorder.status().state, ManualDiagnosticTraceState::Frozen);
    }

    #[test]
    fn document_has_versioned_privacy_friendly_metadata_and_generic_fields() {
        assert_eq!(MANUAL_DIAGNOSTIC_TRACE_SCHEMA_VERSION, 3);
        let origin = Instant::now();
        let mut recorder = recorder_with_limits(8, Duration::from_secs(120));
        recorder.start_at(
            ManualDiagnosticTraceSessionId("redacted-session".to_owned()),
            UNIX_EPOCH + Duration::from_millis(55),
            origin,
            fields([("diagnostics_opt_in", Value::from(true))]),
        );
        assert!(recorder.record_at(
            origin + Duration::from_micros(7),
            "alignment".to_owned(),
            "page_ready".to_owned(),
            fields([
                ("track_number", Value::from(2)),
                ("bytes", Value::from(4096)),
                ("source", Value::from("local alignment source")),
            ]),
        ));
        let document = recorder.json_document();
        assert_eq!(
            document["schema"]["name"],
            MANUAL_DIAGNOSTIC_TRACE_SCHEMA_NAME
        );
        assert_eq!(
            document["schema"]["version"],
            MANUAL_DIAGNOSTIC_TRACE_SCHEMA_VERSION
        );
        assert_eq!(document["session"]["started_unix_ms"], 55);
        assert_eq!(document["session"]["metadata"]["diagnostics_opt_in"], true);
        assert_eq!(document["events"][0]["fields"]["bytes"], 4096);
        let session = document["session"].as_object().expect("session object");
        assert!(!session.contains_key("user"));
        assert!(!session.contains_key("hostname"));
        assert!(!session.contains_key("current_directory"));
        assert!(!session.contains_key("source_path"));
        let payload = recorder.export_payload().expect("serializable trace");
        assert_eq!(payload.media_type, JSON_MEDIA_TYPE);
        assert!(payload.suggested_filename.contains("redacted-session"));
        let exported: Value = serde_json::from_slice(&payload.bytes).expect("valid JSON payload");
        assert_eq!(exported, document);
    }

    #[test]
    fn stop_disables_events_and_start_resets_session_counters() {
        let origin = Instant::now();
        let mut recorder = recorder_with_limits(2, Duration::from_secs(120));
        start_at(&mut recorder, origin);
        assert!(recorder.record_at(
            origin,
            "session".to_owned(),
            "event".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert!(recorder.stop());
        assert_eq!(
            recorder.status().state,
            ManualDiagnosticTraceState::Inactive
        );
        assert!(!recorder.record_at(
            origin,
            "session".to_owned(),
            "dropped".to_owned(),
            ManualDiagnosticTraceFields::new(),
        ));
        assert_eq!(recorder.status().dropped_events, 1);
        start_at(&mut recorder, origin + Duration::from_secs(1));
        let status = recorder.status();
        assert_eq!(status.state, ManualDiagnosticTraceState::Recording);
        assert_eq!(status.retained_events, 0);
        assert_eq!(status.dropped_events, 0);
        assert_eq!(status.overwritten_events, 0);
    }
}
