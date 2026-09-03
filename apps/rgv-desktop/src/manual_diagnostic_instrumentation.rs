use super::*;

const MANUAL_TRACE_STATE_SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const MANUAL_TRACE_WHEEL_IDLE_INTERVAL: Duration = Duration::from_millis(250);

fn diagnostic_caller_file(path: &str) -> String {
    path.rsplit_once("/src/").map_or_else(
        || {
            std::path::Path::new(path)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("unknown")
                .to_owned()
        },
        |(_, suffix)| format!("src/{suffix}"),
    )
}

fn diagnostic_revision_fingerprint(fingerprint: RevisionFingerprint) -> serde_json::Value {
    serde_json::json!({
        "high": fingerprint.high(),
        "low": fingerprint.low(),
    })
}

fn diagnostic_optional_reference_revision(
    revision: Option<ReferenceRevision>,
) -> serde_json::Value {
    revision.map_or(serde_json::Value::Null, |revision| {
        diagnostic_revision_fingerprint(revision.fingerprint())
    })
}

fn diagnostic_alignment_predicate_scope(scope: &AlignmentPredicateScope) -> serde_json::Value {
    match scope {
        AlignmentPredicateScope::Anywhere => serde_json::json!({ "kind": "anywhere" }),
        AlignmentPredicateScope::Position { contig, position } => serde_json::json!({
            "kind": "position",
            "contig": contig,
            "position": position,
        }),
        AlignmentPredicateScope::Interval { contig, start, end } => serde_json::json!({
            "kind": "interval",
            "contig": contig,
            "start": start,
            "end": end,
        }),
    }
}

/// Privacy-preserving predicate representation for saved traces. Numeric thresholds and genomic
/// scopes are useful diagnostic inputs, but auxiliary values can contain sample/read-group names
/// and are therefore represented only by their type and presence.
fn diagnostic_alignment_predicate(predicate: &AlignmentViewPredicate) -> serde_json::Value {
    match predicate {
        AlignmentViewPredicate::MappingQuality { minimum, maximum } => serde_json::json!({
            "kind": "mapping_quality",
            "minimum": minimum,
            "maximum": maximum,
            "requires_reference": false,
        }),
        AlignmentViewPredicate::Flag { flag, expected } => serde_json::json!({
            "kind": "flag",
            "flag": format!("{flag:?}"),
            "expected": expected,
            "requires_reference": false,
        }),
        AlignmentViewPredicate::TemplateLength { minimum, maximum } => serde_json::json!({
            "kind": "template_length",
            "minimum": minimum,
            "maximum": maximum,
            "requires_reference": false,
        }),
        AlignmentViewPredicate::MateContig { contig } => serde_json::json!({
            "kind": "mate_contig",
            "contig": contig,
            "requires_reference": false,
        }),
        AlignmentViewPredicate::Auxiliary {
            tag,
            comparison,
            value,
        } => {
            let value_kind = value.as_ref().map(|value| match value {
                AlignmentAuxFilterValue::Integer(_) => "integer",
                AlignmentAuxFilterValue::FloatBits(_) => "float",
                AlignmentAuxFilterValue::String(_) => "string",
            });
            serde_json::json!({
                "kind": "auxiliary",
                "tag": String::from_utf8_lossy(tag),
                "comparison": format!("{comparison:?}"),
                "value_present": value.is_some(),
                "value_kind": value_kind,
                "value_redacted": value.is_some(),
                "requires_reference": false,
            })
        }
        AlignmentViewPredicate::CigarEvent {
            event,
            scope,
            minimum_length,
            maximum_length,
        } => serde_json::json!({
            "kind": "cigar_event",
            "event": format!("{event:?}"),
            "scope": diagnostic_alignment_predicate_scope(scope),
            "minimum_length": minimum_length,
            "maximum_length": maximum_length,
            "requires_reference": false,
        }),
        AlignmentViewPredicate::AlignedLength { minimum, maximum } => serde_json::json!({
            "kind": "aligned_length",
            "minimum": minimum,
            "maximum": maximum,
            "requires_reference": false,
        }),
        AlignmentViewPredicate::Substitution {
            scope,
            observed_base,
            minimum_count,
            maximum_count,
        } => serde_json::json!({
            "kind": "substitution",
            "scope": diagnostic_alignment_predicate_scope(scope),
            "observed_base": observed_base.map(|base| char::from(base.to_ascii_uppercase()).to_string()),
            "minimum_count": minimum_count,
            "maximum_count": maximum_count,
            "requires_reference": true,
        }),
    }
}

fn diagnostic_alignment_expression(
    version: u16,
    predicates: &[AlignmentViewPredicate],
) -> serde_json::Value {
    serde_json::json!({
        "version": version,
        "predicate_count": predicates.len(),
        "requires_reference": predicates.iter().any(AlignmentViewPredicate::requires_reference),
        "predicates": predicates
            .iter()
            .map(diagnostic_alignment_predicate)
            .collect::<Vec<_>>(),
    })
}

fn diagnostic_alignment_filter_contract(
    contract: &AlignmentViewFilterContract,
    reference: Option<ReferenceRevision>,
) -> serde_json::Value {
    serde_json::json!({
        "contract_fingerprint": contract.fingerprint().get(),
        "evaluation_fingerprint": contract.evaluation_fingerprint(reference).get(),
        "predicate_count": contract.predicates().len(),
        "requires_reference": contract.requires_reference(),
        "reference_revision": diagnostic_optional_reference_revision(reference),
    })
}

fn diagnostic_certified_coverage(
    coverage: &rgv_alignment_scene::CertifiedCoverageSet,
) -> serde_json::Value {
    serde_json::json!({
        "merged_interval_count": coverage.intervals().len(),
        "certificate_count": coverage.certificates().len(),
        "intervals": coverage
            .intervals()
            .iter()
            .map(|interval| serde_json::json!({
                "contig": interval.contig().get(),
                "start": interval.start(),
                "end": interval.end(),
            }))
            .collect::<Vec<_>>(),
    })
}

fn diagnostic_alignment_scene_snapshot(
    snapshot: &AlignmentSceneSnapshot,
    expected_filter: Option<ContractFingerprint>,
    read_authority_scene: Option<rgv_alignment_scene::SceneRevision>,
) -> serde_json::Value {
    let reads = snapshot.reads();
    let evidence = reads.evidence();
    let atlas = reads.atlas();
    let summaries = snapshot.summaries();
    let filter = evidence.view_filter();
    let evidence_filter = filter.fingerprint();
    let atlas_filter = atlas.view_filter();
    let filter_tiles = summaries.filter_tiles();
    let filter_coverage_fingerprint = filter_tiles
        .first()
        .map(|tile| tile.coverage_fingerprint().get());
    let filter_coverage_uniform = filter_tiles
        .iter()
        .all(|tile| Some(tile.coverage_fingerprint().get()) == filter_coverage_fingerprint);
    let filter_tile_evidence_coherent = filter_tiles
        .iter()
        .all(|tile| tile.evidence_revision() == evidence.revision());
    let evidence_atlas_filter_coherent = evidence_filter == atlas_filter;
    let summary_evidence_coherent = summaries.evidence_revision() == evidence.revision();
    let expected_filter_coherent =
        expected_filter.is_none_or(|expected| expected == evidence_filter);
    let read_authority_scene_coherent =
        read_authority_scene.is_none_or(|revision| revision == reads.revision());
    serde_json::json!({
        "snapshot_revision": diagnostic_revision_fingerprint(snapshot.revision().fingerprint()),
        "read_scene_revision": diagnostic_revision_fingerprint(reads.revision().fingerprint()),
        "evidence_revision": diagnostic_revision_fingerprint(evidence.revision().fingerprint()),
        "row_atlas_revision": diagnostic_revision_fingerprint(atlas.revision().fingerprint()),
        "summary_revision": diagnostic_revision_fingerprint(summaries.revision().fingerprint()),
        "record_count": reads.evidence().record_count(),
        "row_count": atlas.row_count(),
        "read_pages": reads.pages().len(),
        "summary_pages": snapshot.summary_pages().len(),
        "filtered_coverage": diagnostic_certified_coverage(evidence.coverage()),
        "eligible_coverage": diagnostic_certified_coverage(evidence.eligible_coverage()),
        "view_filter": {
            "evaluation_fingerprint": evidence_filter.get(),
            "eligible_records": filter.eligible_records(),
            "matching_records": filter.matching_records(),
            "predicate_matches": filter.predicate_matches(),
        },
        "authorities": {
            "expected_filter_evaluation_fingerprint": expected_filter.map(ContractFingerprint::get),
            "row_atlas_filter_evaluation_fingerprint": atlas_filter.get(),
            "filter_provenance_coverage_fingerprint": filter_coverage_fingerprint,
            "filter_provenance_tiles": filter_tiles.len(),
            "filter_provenance_uniform": filter_coverage_uniform,
            "filter_tile_evidence_coherent": filter_tile_evidence_coherent,
            "evidence_atlas_filter_coherent": evidence_atlas_filter_coherent,
            "summary_evidence_coherent": summary_evidence_coherent,
            "expected_filter_coherent": expected_filter_coherent,
            "read_authority_scene_coherent": read_authority_scene_coherent,
            "all_coherent": evidence_atlas_filter_coherent
                && summary_evidence_coherent
                && filter_coverage_uniform
                && filter_tile_evidence_coherent
                && expected_filter_coherent
                && read_authority_scene_coherent,
        },
    })
}

fn diagnostic_alignment_component(component: AlignmentComponent) -> &'static str {
    match component {
        AlignmentComponent::ReadSpan => "read_span",
        AlignmentComponent::AlignedSegment { .. } => "aligned_segment",
        AlignmentComponent::Insertion { .. } => "insertion",
        AlignmentComponent::Deletion { .. } => "deletion",
        AlignmentComponent::ReferenceSkip { .. } => "reference_skip",
        AlignmentComponent::SoftClip { .. } => "soft_clip",
        AlignmentComponent::HardClip { .. } => "hard_clip",
        AlignmentComponent::Padding { .. } => "padding",
        AlignmentComponent::DirectionTerminal => "direction_terminal",
        AlignmentComponent::PairConnector { .. } => "pair_connector",
        AlignmentComponent::Label { .. } => "label",
        AlignmentComponent::ModifiedBase { .. } => "modified_base",
        AlignmentComponent::BisulfiteBase { .. } => "bisulfite_base",
        AlignmentComponent::SmrtKinetics { .. } => "smrt_kinetics",
        AlignmentComponent::UltimaIndel { .. } => "ultima_indel",
        AlignmentComponent::SbxInsertion { .. } => "sbx_insertion",
        AlignmentComponent::SbxTailBase { .. } => "sbx_tail_base",
        AlignmentComponent::SpecialistUnavailable { .. } => "specialist_unavailable",
        AlignmentComponent::Base { .. } => "base",
    }
}

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

impl ManualDiagnosticFramePhase {
    const fn label(self) -> &'static str {
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManualDiagnosticDisplayInvalidation {
    sequence: u64,
    reason: String,
    caller_file: String,
    caller_line: u32,
    caller_column: u32,
    frame_id: Option<u64>,
    after_phase: Option<&'static str>,
    was_already_dirty: bool,
}

impl ManualDiagnosticDisplayInvalidation {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "sequence": self.sequence,
            "reason": self.reason,
            "caller_file": self.caller_file,
            "caller_line": self.caller_line,
            "caller_column": self.caller_column,
            "frame_id": self.frame_id,
            "after_phase": self.after_phase,
            "was_already_dirty": self.was_already_dirty,
        })
    }
}

#[derive(Clone, Debug, Default)]
struct ManualDiagnosticFrameTimings {
    frame_id: u64,
    display_dirty_marks_at_begin: u64,
    display_dirty_clears_at_begin: u64,
    display_dirty_marks_after_first_rebuild: Option<u64>,
    display_dirty_clears_after_first_rebuild: Option<u64>,
    display_dirty_marks_after_shell: Option<u64>,
    display_dirty_clears_after_shell: Option<u64>,
    display_dirty_marks_after_second_rebuild: Option<u64>,
    display_dirty_clears_after_second_rebuild: Option<u64>,
    poll_background_us: u64,
    apply_captured_motion_us: u64,
    first_display_rebuild_us: u64,
    shell_us: u64,
    shell_preparation_us: u64,
    shell_diagnostics_us: u64,
    shell_inspectors_us: u64,
    shell_pair_summary_us: u64,
    shell_scrollbars_us: u64,
    shell_text_copy_us: u64,
    shell_overview_projection_us: u64,
    shell_ui_run_us: u64,
    shell_post_process_us: u64,
    second_display_rebuild_us: u64,
    tessellation_us: u64,
    surface_acquisition_us: u64,
    egui_buffers_us: u64,
    queue_submission_us: u64,
    present_us: u64,
    prefetch_us: u64,
}

struct PendingManualDiagnosticFailure {
    category: String,
    name: String,
    message: String,
}

impl ManualDiagnosticFrameTimings {
    fn add(&mut self, phase: ManualDiagnosticFramePhase, duration: Duration) {
        let micros = duration.as_micros().min(u128::from(u64::MAX)) as u64;
        match phase {
            ManualDiagnosticFramePhase::PollBackground => {
                self.poll_background_us = self.poll_background_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ApplyCapturedMotion => {
                self.apply_captured_motion_us =
                    self.apply_captured_motion_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::FirstDisplayRebuild => {
                self.first_display_rebuild_us =
                    self.first_display_rebuild_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::Shell => {
                self.shell_us = self.shell_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellPreparation => {
                self.shell_preparation_us = self.shell_preparation_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellDiagnostics => {
                self.shell_diagnostics_us = self.shell_diagnostics_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellInspectors => {
                self.shell_inspectors_us = self.shell_inspectors_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellPairSummary => {
                self.shell_pair_summary_us = self.shell_pair_summary_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellScrollbars => {
                self.shell_scrollbars_us = self.shell_scrollbars_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellTextCopy => {
                self.shell_text_copy_us = self.shell_text_copy_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellOverviewProjection => {
                self.shell_overview_projection_us =
                    self.shell_overview_projection_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellUiRun => {
                self.shell_ui_run_us = self.shell_ui_run_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::ShellPostProcess => {
                self.shell_post_process_us = self.shell_post_process_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::SecondDisplayRebuild => {
                self.second_display_rebuild_us =
                    self.second_display_rebuild_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::Tessellation => {
                self.tessellation_us = self.tessellation_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::SurfaceAcquisition => {
                self.surface_acquisition_us = self.surface_acquisition_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::EguiBuffers => {
                self.egui_buffers_us = self.egui_buffers_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::QueueSubmission => {
                self.queue_submission_us = self.queue_submission_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::Present => {
                self.present_us = self.present_us.saturating_add(micros);
            }
            ManualDiagnosticFramePhase::Prefetch => {
                self.prefetch_us = self.prefetch_us.saturating_add(micros);
            }
        }
    }

    fn note_display_invalidation_progress(
        &mut self,
        phase: ManualDiagnosticFramePhase,
        marks: u64,
        clears: u64,
    ) {
        match phase {
            ManualDiagnosticFramePhase::FirstDisplayRebuild => {
                self.display_dirty_marks_after_first_rebuild = Some(marks);
                self.display_dirty_clears_after_first_rebuild = Some(clears);
            }
            ManualDiagnosticFramePhase::Shell => {
                self.display_dirty_marks_after_shell = Some(marks);
                self.display_dirty_clears_after_shell = Some(clears);
            }
            ManualDiagnosticFramePhase::SecondDisplayRebuild => {
                self.display_dirty_marks_after_second_rebuild = Some(marks);
                self.display_dirty_clears_after_second_rebuild = Some(clears);
            }
            ManualDiagnosticFramePhase::PollBackground
            | ManualDiagnosticFramePhase::ApplyCapturedMotion
            | ManualDiagnosticFramePhase::ShellPreparation
            | ManualDiagnosticFramePhase::ShellDiagnostics
            | ManualDiagnosticFramePhase::ShellInspectors
            | ManualDiagnosticFramePhase::ShellPairSummary
            | ManualDiagnosticFramePhase::ShellScrollbars
            | ManualDiagnosticFramePhase::ShellTextCopy
            | ManualDiagnosticFramePhase::ShellOverviewProjection
            | ManualDiagnosticFramePhase::ShellUiRun
            | ManualDiagnosticFramePhase::ShellPostProcess
            | ManualDiagnosticFramePhase::Tessellation
            | ManualDiagnosticFramePhase::SurfaceAcquisition
            | ManualDiagnosticFramePhase::EguiBuffers
            | ManualDiagnosticFramePhase::QueueSubmission
            | ManualDiagnosticFramePhase::Present
            | ManualDiagnosticFramePhase::Prefetch => {}
        }
    }
}

pub(crate) struct ManualDiagnosticTraceRuntime {
    output_directory: PathBuf,
    gpu_adapter: String,
    gpu_backend: String,
    pub(crate) last_saved_path: Option<PathBuf>,
    pub(crate) failure: Option<String>,
    frame_clock: u64,
    current_frame: Option<ManualDiagnosticFrameTimings>,
    pointer_interaction: Option<ManualDiagnosticInteractionId>,
    wheel_interaction: Option<ManualDiagnosticInteractionId>,
    wheel_last_event: Option<Instant>,
    last_state_sample: Option<Instant>,
    last_state: Option<serde_json::Value>,
    last_observed_data_status: String,
    pending_critical_failure: Option<PendingManualDiagnosticFailure>,
    display_dirty_mark_sequence: u64,
    display_dirty_clear_sequence: u64,
    last_completed_frame_phase: Option<&'static str>,
    last_display_invalidation: Option<ManualDiagnosticDisplayInvalidation>,
}

impl ManualDiagnosticTraceRuntime {
    pub(crate) fn new(output_directory: PathBuf, gpu_adapter: String, gpu_backend: String) -> Self {
        Self {
            output_directory,
            gpu_adapter,
            gpu_backend,
            last_saved_path: None,
            failure: None,
            frame_clock: 0,
            current_frame: None,
            pointer_interaction: None,
            wheel_interaction: None,
            wheel_last_event: None,
            last_state_sample: None,
            last_state: None,
            last_observed_data_status: String::new(),
            pending_critical_failure: None,
            display_dirty_mark_sequence: 0,
            display_dirty_clear_sequence: 0,
            last_completed_frame_phase: None,
            last_display_invalidation: None,
        }
    }

    fn reset_for_recording(&mut self, current_data_status: &str) {
        self.last_saved_path = None;
        self.failure = None;
        self.current_frame = None;
        self.pointer_interaction = None;
        self.wheel_interaction = None;
        self.wheel_last_event = None;
        self.last_state_sample = None;
        self.last_state = None;
        self.last_observed_data_status = current_data_status.to_owned();
        self.pending_critical_failure = None;
        self.display_dirty_mark_sequence = 0;
        self.display_dirty_clear_sequence = 0;
        self.last_completed_frame_phase = None;
        self.last_display_invalidation = None;
    }
}

impl Viewer {
    /// Marks the generic display derivative stale and, while a manual trace is recording,
    /// preserves the exact producer and frame phase. Recording repeated marks while already dirty
    /// is intentional: those marks reveal the feedback loop that can re-dirty a just-rebuilt
    /// display every frame.
    #[track_caller]
    pub(crate) fn mark_display_dirty(&mut self, reason: &'static str) {
        let was_already_dirty = self.display_dirty;
        self.display_dirty = true;
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        let caller = std::panic::Location::caller();
        let runtime = &mut self.manual_diagnostic_trace_runtime;
        runtime.display_dirty_mark_sequence = runtime.display_dirty_mark_sequence.saturating_add(1);
        let invalidation = ManualDiagnosticDisplayInvalidation {
            sequence: runtime.display_dirty_mark_sequence,
            reason: reason.to_owned(),
            caller_file: diagnostic_caller_file(caller.file()),
            caller_line: caller.line(),
            caller_column: caller.column(),
            frame_id: runtime.current_frame.as_ref().map(|frame| frame.frame_id),
            after_phase: runtime.last_completed_frame_phase,
            was_already_dirty,
        };
        runtime.last_display_invalidation = Some(invalidation.clone());
        let interaction_id = self
            .manual_diagnostic_trace_runtime
            .pointer_interaction
            .or(self.manual_diagnostic_trace_runtime.wheel_interaction)
            .map(ManualDiagnosticInteractionId::get);
        self.manual_diagnostic_trace.record(
            "display_invalidation",
            "marked_dirty",
            object_fields(serde_json::json!({
                "causal_interaction_id": interaction_id,
                "display_dirty_after": self.display_dirty,
                "invalidation": invalidation.json(),
            })),
        );
    }

    /// Clears display invalidation only after the complete CPU/GPU display derivative has been
    /// rebuilt. The clear sequence is paired with mark sequences in per-frame diagnostics.
    #[track_caller]
    pub(crate) fn clear_display_dirty(&mut self, reason: &'static str) {
        let was_dirty = self.display_dirty;
        self.display_dirty = false;
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        let caller = std::panic::Location::caller();
        let runtime = &mut self.manual_diagnostic_trace_runtime;
        runtime.display_dirty_clear_sequence =
            runtime.display_dirty_clear_sequence.saturating_add(1);
        let frame_id = self
            .manual_diagnostic_trace_runtime
            .current_frame
            .as_ref()
            .map(|frame| frame.frame_id);
        let after_phase = self
            .manual_diagnostic_trace_runtime
            .last_completed_frame_phase;
        self.manual_diagnostic_trace.record(
            "display_invalidation",
            "cleared_dirty",
            object_fields(serde_json::json!({
                "sequence": self.manual_diagnostic_trace_runtime.display_dirty_clear_sequence,
                "reason": reason,
                "caller_file": diagnostic_caller_file(caller.file()),
                "caller_line": caller.line(),
                "caller_column": caller.column(),
                "frame_id": frame_id,
                "after_phase": after_phase,
                "was_dirty": was_dirty,
                "display_dirty_after": self.display_dirty,
            })),
        );
    }

    pub(crate) fn maybe_start_pending_manual_diagnostic_trace(&mut self) {
        if !self.manual_diagnostic_trace_autostart_pending {
            return;
        }
        let source_is_ready = self.synthetic
            || self.alignment_tracks.is_empty()
            || self
                .alignment_tracks
                .iter()
                .any(|track| track.dictionary.is_some());
        let bootstrap_locus_is_resolved = self.synthetic
            || self.alignment_tracks.is_empty()
            || self.panels.active().contig_name != "chrSynthetic";
        if source_is_ready && bootstrap_locus_is_resolved {
            self.manual_diagnostic_trace_autostart_pending = false;
            self.start_manual_diagnostic_trace("launch option");
        }
    }

    pub(crate) fn start_manual_diagnostic_trace(&mut self, trigger: &str) {
        self.manual_diagnostic_trace_runtime
            .reset_for_recording(&self.data_status);
        let panel = self.panels.active();
        let source_kinds = self
            .alignment_tracks
            .iter()
            .map(|track| {
                serde_json::json!({
                    "track_id": track.config.id.get(),
                    "source_kind": match &track.config.source {
                        TrackSource::Local { .. } => "local",
                        TrackSource::Remote { .. } => "remote",
                    },
                    "maximum_alignments": track.config.maximum_alignments,
                })
            })
            .collect::<Vec<_>>();
        let metadata = object_fields(serde_json::json!({
            "trigger": trigger,
            "application_version": env!("CARGO_PKG_VERSION"),
            "application_instance_id": self.application_instance_id,
            "operating_system": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "gpu_adapter": self.manual_diagnostic_trace_runtime.gpu_adapter,
            "gpu_backend": self.manual_diagnostic_trace_runtime.gpu_backend,
            "surface_width_pixels": self.surface_config.width,
            "surface_height_pixels": self.surface_config.height,
            "scale_factor": self.window.scale_factor(),
            "cache_budget_mib": self.settings.cache_budget_mib,
            "read_browsing_buffer_percent": self.settings.read_browsing_buffer_percent,
            "workspace_alignment_limit": self.workspace_alignment_limit,
            "active_locus": {
                "panel_id": panel.id.get(),
                "contig": panel.contig_name,
                "start": panel.viewport.interval().start(),
                "end": panel.viewport.interval().end(),
                "exact_start": panel.viewport.exact_start(),
                "span": panel.viewport.span(),
                "pixel_width": panel.viewport.pixel_width(),
            },
            "alignment_tracks": source_kinds,
            "capabilities": [
                "alignment_navigation_and_publication",
                "alignment_filter_and_highlight_authority",
                "alignment_context_menu_actions",
                "retained_evidence_source_reuse",
                "all_panel_read_scroll_authority",
                "base_label_projection",
                "reference_filter_dependency",
                "display_invalidation_provenance",
                "scroll_cache_authority",
                "renderer_invariant_failures",
                "ledger_retained_breakdown",
            ],
            "privacy": "No sequence, read names, auxiliary-tag values, credentials, or remote URLs are intentionally recorded. Filter kinds, numeric thresholds, redacted auxiliary value types, user-visible diagnostic/status text, and genomic loci are included.",
        }));
        let session = self.manual_diagnostic_trace.start_with_metadata(metadata);
        tracing::info!(
            session_id = session.as_str(),
            output_directory = %self.manual_diagnostic_trace_runtime.output_directory.display(),
            "manual diagnostic trace started"
        );
        self.diagnostics_open = true;
        self.window.request_redraw();
    }

    pub(crate) fn stop_and_save_manual_diagnostic_trace(&mut self) {
        if !self.manual_diagnostic_trace.stop() {
            return;
        }
        self.save_manual_diagnostic_trace("manual stop");
        self.window.request_redraw();
    }

    pub(crate) fn save_recording_manual_diagnostic_trace_before_exit(&mut self) {
        if matches!(
            self.manual_diagnostic_trace.status().state,
            ManualDiagnosticTraceState::Recording
        ) {
            let fields = object_fields(serde_json::json!({ "reason": "application exit" }));
            self.manual_diagnostic_trace
                .record("lifecycle", "application_exit", fields);
            let _ = self.manual_diagnostic_trace.stop();
            self.save_manual_diagnostic_trace("application exit");
        } else {
            self.save_frozen_manual_diagnostic_trace_if_needed();
        }
    }

    pub(crate) fn manual_diagnostic_trace_snapshot(&self) -> ui::ManualInteractionTraceSnapshot {
        let status = self.manual_diagnostic_trace.status();
        let retained_duration = match (status.first_elapsed_us, status.last_elapsed_us) {
            (Some(first), Some(last)) => Duration::from_micros(last.saturating_sub(first)),
            _ => Duration::ZERO,
        };
        ui::ManualInteractionTraceSnapshot {
            state: match status.state {
                ManualDiagnosticTraceState::Inactive => ui::ManualInteractionTraceState::Idle,
                ManualDiagnosticTraceState::Recording => ui::ManualInteractionTraceState::Recording,
                ManualDiagnosticTraceState::Frozen => ui::ManualInteractionTraceState::Frozen,
            },
            event_count: status.retained_events,
            dropped_event_count: status
                .dropped_events
                .saturating_add(status.overwritten_events),
            retained_duration,
            saved_path: self
                .manual_diagnostic_trace_runtime
                .last_saved_path
                .as_ref()
                .map(|path| path.display().to_string()),
            failure: self.manual_diagnostic_trace_runtime.failure.clone(),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn record_manual_window_event(&mut self, event: &WindowEvent, consumed: bool) {
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        self.finish_idle_manual_wheel_interaction();
        let pointer = || {
            serde_json::json!({
                "x_physical": self.cursor.x,
                "y_physical": self.cursor.y,
                "scale_factor": self.window.scale_factor(),
                "egui_consumed": consumed,
            })
        };
        match event {
            WindowEvent::MouseInput { state, button, .. } => {
                let mut fields = object_fields(pointer());
                fields.insert(
                    "button".to_owned(),
                    serde_json::json!(format!("{button:?}")),
                );
                fields.insert("state".to_owned(), serde_json::json!(format!("{state:?}")));
                if *button == MouseButton::Left && *state == ElementState::Pressed {
                    if let Some(previous) = self
                        .manual_diagnostic_trace_runtime
                        .pointer_interaction
                        .take()
                    {
                        self.manual_diagnostic_trace.end_interaction(
                            previous,
                            "input",
                            "pointer_superseded",
                            ManualDiagnosticTraceFields::new(),
                        );
                    }
                    self.manual_diagnostic_trace_runtime.pointer_interaction = self
                        .manual_diagnostic_trace
                        .begin_interaction("input", "pointer_press", fields);
                } else if *button == MouseButton::Left && *state == ElementState::Released {
                    if let Some(id) = self
                        .manual_diagnostic_trace_runtime
                        .pointer_interaction
                        .take()
                    {
                        self.manual_diagnostic_trace.end_interaction(
                            id,
                            "input",
                            "pointer_release",
                            fields,
                        );
                    } else {
                        self.manual_diagnostic_trace.record(
                            "input",
                            "pointer_release_without_press",
                            fields,
                        );
                    }
                } else {
                    self.manual_diagnostic_trace
                        .record("input", "mouse_button", fields);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(id) = self.manual_diagnostic_trace_runtime.pointer_interaction {
                    let fields = object_fields(serde_json::json!({
                        "x_physical": position.x,
                        "y_physical": position.y,
                        "egui_consumed": consumed,
                        "canvas_dragging": self.dragging,
                        "vertical_scrollbar_dragging": self.vertical_overview_drag.is_some(),
                    }));
                    self.manual_diagnostic_trace.record_interaction(
                        id,
                        "input",
                        "pointer_move",
                        fields,
                    );
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                if self
                    .manual_diagnostic_trace_runtime
                    .wheel_interaction
                    .is_none()
                {
                    self.manual_diagnostic_trace_runtime.wheel_interaction =
                        self.manual_diagnostic_trace.begin_interaction(
                            "input",
                            "wheel_begin",
                            object_fields(serde_json::json!({
                                "x_physical": self.cursor.x,
                                "y_physical": self.cursor.y,
                                "shift": self.modifiers.shift_key(),
                            })),
                        );
                }
                self.manual_diagnostic_trace_runtime.wheel_last_event = Some(Instant::now());
                let (delta_kind, delta_x, delta_y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ("line", f64::from(*x), f64::from(*y)),
                    MouseScrollDelta::PixelDelta(position) => ("pixel", position.x, position.y),
                };
                let fields = object_fields(serde_json::json!({
                    "delta_kind": delta_kind,
                    "delta_x": delta_x,
                    "delta_y": delta_y,
                    "phase": format!("{phase:?}"),
                    "egui_consumed": consumed,
                    "shift": self.modifiers.shift_key(),
                    "cursor_in_genomic_canvas": self.cursor_in_genomic_canvas(),
                }));
                if let Some(id) = self.manual_diagnostic_trace_runtime.wheel_interaction {
                    self.manual_diagnostic_trace.record_interaction(
                        id,
                        "input",
                        "wheel_delta",
                        fields,
                    );
                    if matches!(
                        phase,
                        winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                    ) {
                        self.manual_diagnostic_trace.end_interaction(
                            id,
                            "input",
                            "wheel_end",
                            ManualDiagnosticTraceFields::new(),
                        );
                        self.manual_diagnostic_trace_runtime.wheel_interaction = None;
                        self.manual_diagnostic_trace_runtime.wheel_last_event = None;
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let fields = object_fields(serde_json::json!({
                    "key": format!("{:?}", event.logical_key),
                    "repeat": event.repeat,
                    "egui_consumed": consumed,
                    "control": self.modifiers.control_key(),
                    "super": self.modifiers.super_key(),
                    "shift": self.modifiers.shift_key(),
                    "alt": self.modifiers.alt_key(),
                }));
                if let Some(id) = self.manual_diagnostic_trace.begin_interaction(
                    "input",
                    "keyboard_press",
                    fields,
                ) {
                    self.manual_diagnostic_trace.end_interaction(
                        id,
                        "input",
                        "keyboard_dispatch",
                        ManualDiagnosticTraceFields::new(),
                    );
                }
            }
            WindowEvent::Resized(size) => {
                self.manual_diagnostic_trace.record(
                    "window",
                    "resized",
                    object_fields(serde_json::json!({
                        "width": size.width,
                        "height": size.height,
                    })),
                );
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.manual_diagnostic_trace.record(
                    "window",
                    "scale_factor_changed",
                    object_fields(serde_json::json!({ "scale_factor": scale_factor })),
                );
            }
            WindowEvent::Focused(focused) => {
                self.manual_diagnostic_trace.record(
                    "window",
                    "focus_changed",
                    object_fields(serde_json::json!({ "focused": focused })),
                );
            }
            WindowEvent::RedrawRequested => {
                self.manual_diagnostic_trace.record(
                    "frame",
                    "redraw_requested",
                    ManualDiagnosticTraceFields::new(),
                );
            }
            _ => {}
        }
    }

    pub(crate) fn record_manual_device_event(&mut self, event: &DeviceEvent) {
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        if let DeviceEvent::MouseMotion { delta } = event
            && let Some(id) = self.manual_diagnostic_trace_runtime.pointer_interaction
        {
            self.manual_diagnostic_trace.record_interaction(
                id,
                "input",
                "raw_mouse_motion",
                object_fields(serde_json::json!({
                    "delta_x": delta.0,
                    "delta_y": delta.1,
                    "overview_capture": format!("{:?}", self.overview_capture),
                })),
            );
        }
    }

    pub(crate) fn begin_manual_diagnostic_frame(&mut self) {
        if !self.manual_diagnostic_trace_is_recording() {
            self.manual_diagnostic_trace_runtime.current_frame = None;
            return;
        }
        self.finish_idle_manual_wheel_interaction();
        self.manual_diagnostic_trace_runtime.frame_clock = self
            .manual_diagnostic_trace_runtime
            .frame_clock
            .saturating_add(1);
        let frame_id = self.manual_diagnostic_trace_runtime.frame_clock;
        self.manual_diagnostic_trace_runtime
            .last_completed_frame_phase = Some("frame_begin");
        self.manual_diagnostic_trace_runtime.current_frame = Some(ManualDiagnosticFrameTimings {
            frame_id,
            display_dirty_marks_at_begin: self
                .manual_diagnostic_trace_runtime
                .display_dirty_mark_sequence,
            display_dirty_clears_at_begin: self
                .manual_diagnostic_trace_runtime
                .display_dirty_clear_sequence,
            ..ManualDiagnosticFrameTimings::default()
        });
        self.manual_diagnostic_trace.record(
            "frame",
            "begin",
            object_fields(serde_json::json!({
                "frame_id": frame_id,
                "display_dirty": self.display_dirty,
                "publication_retry_pending": self.alignment_gpu_publication_retry.is_some(),
            })),
        );
    }

    pub(crate) fn record_manual_diagnostic_frame_phase(
        &mut self,
        phase: ManualDiagnosticFramePhase,
        duration: Duration,
    ) {
        let marks = self
            .manual_diagnostic_trace_runtime
            .display_dirty_mark_sequence;
        let clears = self
            .manual_diagnostic_trace_runtime
            .display_dirty_clear_sequence;
        if let Some(frame) = self.manual_diagnostic_trace_runtime.current_frame.as_mut() {
            frame.add(phase, duration);
            frame.note_display_invalidation_progress(phase, marks, clears);
        }
        self.manual_diagnostic_trace_runtime
            .last_completed_frame_phase = Some(phase.label());
    }

    pub(crate) fn finish_manual_diagnostic_frame(&mut self, total: Duration) {
        let Some(frame) = self.manual_diagnostic_trace_runtime.current_frame.take() else {
            self.save_frozen_manual_diagnostic_trace_if_needed();
            return;
        };
        let interaction_id = self
            .manual_diagnostic_trace_runtime
            .pointer_interaction
            .or(self.manual_diagnostic_trace_runtime.wheel_interaction)
            .map(ManualDiagnosticInteractionId::get);
        let marks_at_end = self
            .manual_diagnostic_trace_runtime
            .display_dirty_mark_sequence;
        let clears_at_end = self
            .manual_diagnostic_trace_runtime
            .display_dirty_clear_sequence;
        let marks_after_first = frame
            .display_dirty_marks_after_first_rebuild
            .unwrap_or(frame.display_dirty_marks_at_begin);
        let clears_after_first = frame
            .display_dirty_clears_after_first_rebuild
            .unwrap_or(frame.display_dirty_clears_at_begin);
        let marks_after_shell = frame
            .display_dirty_marks_after_shell
            .unwrap_or(marks_after_first);
        let clears_after_shell = frame
            .display_dirty_clears_after_shell
            .unwrap_or(clears_after_first);
        let marks_after_second = frame
            .display_dirty_marks_after_second_rebuild
            .unwrap_or(marks_after_shell);
        let clears_after_second = frame
            .display_dirty_clears_after_second_rebuild
            .unwrap_or(clears_after_shell);
        let last_display_invalidation = self
            .manual_diagnostic_trace_runtime
            .last_display_invalidation
            .as_ref()
            .map_or(
                serde_json::Value::Null,
                ManualDiagnosticDisplayInvalidation::json,
            );
        self.manual_diagnostic_trace.record(
            "frame",
            "present_cycle_finished",
            object_fields(serde_json::json!({
                "frame_id": frame.frame_id,
                "causal_interaction_id": interaction_id,
                "total_us": duration_micros(total),
                "poll_background_us": frame.poll_background_us,
                "apply_captured_motion_us": frame.apply_captured_motion_us,
                "first_display_rebuild_us": frame.first_display_rebuild_us,
                "shell_us": frame.shell_us,
                "shell_preparation_us": frame.shell_preparation_us,
                "shell_diagnostics_us": frame.shell_diagnostics_us,
                "shell_inspectors_us": frame.shell_inspectors_us,
                "shell_pair_summary_us": frame.shell_pair_summary_us,
                "shell_scrollbars_us": frame.shell_scrollbars_us,
                "shell_text_copy_us": frame.shell_text_copy_us,
                "shell_overview_projection_us": frame.shell_overview_projection_us,
                "shell_ui_run_us": frame.shell_ui_run_us,
                "shell_post_process_us": frame.shell_post_process_us,
                "second_display_rebuild_us": frame.second_display_rebuild_us,
                "tessellation_us": frame.tessellation_us,
                "surface_acquisition_us": frame.surface_acquisition_us,
                "egui_buffers_us": frame.egui_buffers_us,
                "queue_submission_us": frame.queue_submission_us,
                "present_us": frame.present_us,
                "prefetch_us": frame.prefetch_us,
                "display_dirty_after": self.display_dirty,
                "display_invalidation": {
                    "mark_sequence_at_begin": frame.display_dirty_marks_at_begin,
                    "mark_sequence_at_end": marks_at_end,
                    "marks_before_or_during_first_rebuild": marks_after_first.saturating_sub(frame.display_dirty_marks_at_begin),
                    "marks_during_shell": marks_after_shell.saturating_sub(marks_after_first),
                    "marks_during_second_rebuild": marks_after_second.saturating_sub(marks_after_shell),
                    "marks_after_second_rebuild": marks_at_end.saturating_sub(marks_after_second),
                    "clear_sequence_at_begin": frame.display_dirty_clears_at_begin,
                    "clear_sequence_at_end": clears_at_end,
                    "clears_before_or_during_first_rebuild": clears_after_first.saturating_sub(frame.display_dirty_clears_at_begin),
                    "clears_during_shell": clears_after_shell.saturating_sub(clears_after_first),
                    "clears_during_second_rebuild": clears_after_second.saturating_sub(clears_after_shell),
                    "clears_after_second_rebuild": clears_at_end.saturating_sub(clears_after_second),
                    "last": last_display_invalidation,
                },
            })),
        );
        self.record_manual_diagnostic_state_if_needed(frame.frame_id);
        if let Some(failure) = self
            .manual_diagnostic_trace_runtime
            .pending_critical_failure
            .take()
        {
            self.freeze_manual_diagnostic_trace(
                &failure.category,
                &failure.name,
                &failure.message,
                ManualDiagnosticTraceFields::new(),
            );
        }
        self.record_manual_data_status_change();
        self.save_frozen_manual_diagnostic_trace_if_needed();
        self.manual_diagnostic_trace_runtime
            .last_completed_frame_phase = Some("frame_finished");
    }

    pub(crate) fn record_manual_navigation_intent(&mut self, kind: &str) {
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        let panel = self.panels.active();
        let causal_interaction_id = self
            .manual_diagnostic_trace_runtime
            .pointer_interaction
            .or(self.manual_diagnostic_trace_runtime.wheel_interaction)
            .map(ManualDiagnosticInteractionId::get);
        self.manual_diagnostic_trace.record(
            "navigation",
            kind,
            object_fields(serde_json::json!({
                "panel_id": panel.id.get(),
                "contig": panel.contig_name,
                "start": panel.viewport.interval().start(),
                "end": panel.viewport.interval().end(),
                "exact_start": panel.viewport.exact_start(),
                "span": panel.viewport.span(),
                "pixel_width": panel.viewport.pixel_width(),
                "causal_interaction_id": causal_interaction_id,
                "dragging": self.dragging,
                "horizontal_overview_drag": self.horizontal_overview_drag.is_some(),
                "vertical_overview_drag": self.vertical_overview_drag.is_some(),
            })),
        );
    }

    pub(crate) fn record_manual_diagnostic_event(
        &mut self,
        category: &str,
        name: &str,
        fields: serde_json::Value,
    ) {
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        self.manual_diagnostic_trace
            .record(category, name, object_fields(fields));
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_manual_vertical_scroll_intent(
        &mut self,
        track_id: TrackId,
        requested: f32,
        previous: f32,
        maximum: f32,
        applied: f32,
        source: &str,
    ) {
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        let causal_interaction_id = self
            .manual_diagnostic_trace_runtime
            .pointer_interaction
            .or(self.manual_diagnostic_trace_runtime.wheel_interaction)
            .map(ManualDiagnosticInteractionId::get);
        self.manual_diagnostic_trace.record(
            "vertical_navigation",
            "scroll_intent",
            object_fields(serde_json::json!({
                "panel_id": self.panels.active().id.get(),
                "track_id": track_id.get(),
                "source": source,
                "causal_interaction_id": causal_interaction_id,
                "requested_points": requested,
                "previous_points": previous,
                "maximum_points": maximum,
                "applied_points": applied,
                "row_authority": format!("{:?}", self.read_panel_row_authority(track_id)),
            })),
        );
    }

    pub(crate) fn freeze_manual_diagnostic_trace(
        &mut self,
        category: &str,
        name: &str,
        message: &str,
        mut fields: ManualDiagnosticTraceFields,
    ) {
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        fields.insert("message".to_owned(), serde_json::json!(message));
        fields.insert(
            "viewer_state".to_owned(),
            self.manual_diagnostic_state_snapshot(),
        );
        if self
            .manual_diagnostic_trace
            .freeze_on_critical_failure(category, name, fields)
        {
            self.manual_diagnostic_trace_runtime.failure = Some(message.to_owned());
            self.save_frozen_manual_diagnostic_trace_if_needed();
            self.diagnostics_open = true;
            self.window.request_redraw();
        }
    }

    pub(crate) fn queue_manual_diagnostic_trace_failure(
        &mut self,
        category: &str,
        name: &str,
        message: &str,
        fields: ManualDiagnosticTraceFields,
    ) {
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        self.manual_diagnostic_trace
            .record(category, format!("{name}_detected"), fields);
        self.manual_diagnostic_trace_runtime
            .pending_critical_failure = Some(PendingManualDiagnosticFailure {
            category: category.to_owned(),
            name: name.to_owned(),
            message: message.to_owned(),
        });
    }

    pub(crate) fn manual_diagnostic_trace_is_recording(&self) -> bool {
        matches!(
            self.manual_diagnostic_trace.status().state,
            ManualDiagnosticTraceState::Recording
        )
    }

    fn finish_idle_manual_wheel_interaction(&mut self) {
        let expired = self
            .manual_diagnostic_trace_runtime
            .wheel_last_event
            .is_some_and(|last| last.elapsed() >= MANUAL_TRACE_WHEEL_IDLE_INTERVAL);
        if !expired {
            return;
        }
        if let Some(id) = self
            .manual_diagnostic_trace_runtime
            .wheel_interaction
            .take()
        {
            self.manual_diagnostic_trace.end_interaction(
                id,
                "input",
                "wheel_idle_end",
                ManualDiagnosticTraceFields::new(),
            );
        }
        self.manual_diagnostic_trace_runtime.wheel_last_event = None;
    }

    fn record_manual_diagnostic_state_if_needed(&mut self, frame_id: u64) {
        if !self.manual_diagnostic_trace_is_recording() {
            return;
        }
        let now = Instant::now();
        let interval_elapsed = self
            .manual_diagnostic_trace_runtime
            .last_state_sample
            .is_none_or(|last| {
                now.saturating_duration_since(last) >= MANUAL_TRACE_STATE_SAMPLE_INTERVAL
            });
        if !interval_elapsed {
            return;
        }
        let state = self.manual_diagnostic_state_snapshot();
        let changed = self.manual_diagnostic_trace_runtime.last_state.as_ref() != Some(&state);
        self.manual_diagnostic_trace_runtime.last_state = Some(state.clone());
        self.manual_diagnostic_trace_runtime.last_state_sample = Some(now);
        self.manual_diagnostic_trace.record(
            "state",
            if changed { "changed" } else { "periodic" },
            object_fields(serde_json::json!({
                "frame_id": frame_id,
                "snapshot": state,
            })),
        );
    }

    /// Complete privacy-preserving filter authority for one panel/track pair. Context-menu
    /// events and periodic snapshots share this representation so a requested mutation can be
    /// followed through worker staging and atomic publication without consulting MCP after a
    /// lock-up.
    pub(crate) fn alignment_filter_diagnostic_state(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> serde_json::Value {
        if !self.manual_diagnostic_trace_is_recording() {
            return serde_json::Value::Null;
        }
        let Some(track) = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
        else {
            return serde_json::Value::Null;
        };
        let effective_view =
            alignment_effective_view_predicates(&track.config.filters, &track.active_filters);
        let configured = serde_json::json!({
            "view": diagnostic_alignment_expression(
                track.config.filters.alignment_view_filter.version,
                &effective_view,
            ),
            "highlight": diagnostic_alignment_expression(
                track.config.filters.alignment_highlight_filter.version,
                &track.config.filters.alignment_highlight_filter.all,
            ),
        });
        let projection = track.projection(panel_id);
        let requested = projection
            .and_then(|projection| projection.scene_contract.as_ref())
            .map_or(serde_json::Value::Null, |contract| {
                serde_json::json!({
                    "view": diagnostic_alignment_filter_contract(
                        &contract.view_filter,
                        contract.presentation.reference,
                    ),
                    "highlight": diagnostic_alignment_filter_contract(
                        &contract.highlight_filter,
                        contract.presentation.reference,
                    ),
                    "scene_reference_revision": diagnostic_optional_reference_revision(
                        contract.presentation.reference,
                    ),
                    "reference_authority_embedded_in_requested_contract": contract.reference.is_some(),
                })
            });
        let expected_filter = projection
            .and_then(|projection| projection.scene_contract.as_ref())
            .map(|contract| {
                contract
                    .view_filter
                    .evaluation_fingerprint(contract.presentation.reference)
            });
        let staged = projection
            .and_then(|projection| projection.scene_frame.as_ref())
            .and_then(|frame| frame.snapshot.as_ref().map(|snapshot| (frame, snapshot)))
            .map_or(serde_json::Value::Null, |(frame, snapshot)| {
                serde_json::json!({
                    "scene_contracts_current": frame.scene_contracts_current,
                    "snapshot": diagnostic_alignment_scene_snapshot(
                        snapshot,
                        expected_filter,
                        None,
                    ),
                })
            });
        let committed = self
            .presented_alignment_lane(panel_id, track_id)
            .and_then(|presented| {
                presented
                    .lane
                    .snapshot
                    .as_ref()
                    .map(|snapshot| (presented, snapshot))
            })
            .map_or(serde_json::Value::Null, |(presented, snapshot)| {
                serde_json::json!({
                    "target_revision": presented.target_revision,
                    "scene_contracts_current": presented.lane.scene_contracts_current,
                    "source_request_token": presented.source.source_request_token.get(),
                    "desired_request_token": presented.source.desired_request_token.get(),
                    "reference_ready": presented.source.reference_ready,
                    "snapshot": diagnostic_alignment_scene_snapshot(
                        snapshot,
                        expected_filter,
                        presented.read_authority.map(|authority| authority.scene_revision),
                    ),
                })
            });
        let highlight = projection.map_or(serde_json::Value::Null, |projection| {
            serde_json::json!({
                "cache_key": projection.scene_highlight_key.map(|key| {
                    serde_json::json!({
                        "evidence_revision": diagnostic_revision_fingerprint(key.evidence_revision.fingerprint()),
                        "target": {
                            "contig": key.target.contig().get(),
                            "start": key.target.start(),
                            "end": key.target.end(),
                        },
                        "filter_contract_fingerprint": key.filter.get(),
                        // Retained for schema-v2 trace consumers; historically this field named
                        // the contract component while reference revision was reported beside it.
                        "filter_evaluation_fingerprint": key.filter.get(),
                        "reference_revision": diagnostic_optional_reference_revision(
                            key.reference.map(|(revision, _)| revision),
                        ),
                        "reference_interval": key.reference.map(|(_, interval)| serde_json::json!({
                            "contig": interval.contig().get(),
                            "start": interval.start(),
                            "end": interval.end(),
                        })),
                    })
                }),
                "matching_handles": projection.scene_highlighted.len(),
                "interaction_bound_handles": projection.scene_interaction.highlighted().len(),
                "status": projection.scene_highlight_status,
            })
        });
        let reference_install_receipt = projection
            .and_then(|projection| projection.scene_owner)
            .and_then(|owner| {
                self.panels.get(panel_id).and_then(|panel| {
                    panel
                        .reference_installs_queued
                        .get(&owner)
                        .map(|(revision, interval)| {
                            serde_json::json!({
                                "owner": owner.get(),
                                "revision": diagnostic_revision_fingerprint(revision.fingerprint()),
                                "interval": {
                                    "contig": interval.contig().get(),
                                    "start": interval.start(),
                                    "end": interval.end(),
                                },
                            })
                        })
                })
            })
            .unwrap_or(serde_json::Value::Null);
        let panel_reference = self.panels.get(panel_id).map_or(
            serde_json::Value::Null,
            |panel| {
                serde_json::json!({
                    "viewer_revision": diagnostic_optional_reference_revision(self.reference_revision),
                    "slice_revision": diagnostic_optional_reference_revision(panel.reference_slice_revision),
                    "request_revision": diagnostic_optional_reference_revision(panel.reference_request_revision),
                    "request_interval": panel.reference_request_interval.map(|interval| {
                        serde_json::json!({
                            "contig": interval.contig().get(),
                            "start": interval.start(),
                            "end": interval.end(),
                        })
                    }),
                    "slice_interval": panel.reference_slice.as_ref().map(|slice| {
                        serde_json::json!({
                            "contig": slice.interval.contig().get(),
                            "start": slice.interval.start(),
                            "end": slice.interval.end(),
                        })
                    }),
                    "install_receipt": reference_install_receipt,
                })
            },
        );
        serde_json::json!({
            "configured": configured,
            "requested": requested,
            "staged": staged,
            "committed": committed,
            "highlight": highlight,
            "reference": panel_reference,
            "source_acquisitions": projection.map_or(0, |projection| projection.scene_telemetry.source_acquisitions),
        })
    }

    fn alignment_base_label_diagnostic_state(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> serde_json::Value {
        let Some(bundle) = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .filter(|bundle| {
                alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
            })
        else {
            return serde_json::Value::Null;
        };
        let Some((index, authority)) =
            bundle
                .read_authorities
                .iter()
                .enumerate()
                .find(|(_, authority)| {
                    authority.panel_id == panel_id && authority.track_id == track_id
                })
        else {
            return serde_json::Value::Null;
        };
        let Some(labels) = bundle
            .reads
            .as_ref()
            .and_then(|frame| frame.label_frames().get(index))
        else {
            return serde_json::Value::Null;
        };
        let presented_base_labels = labels
            .labels
            .iter()
            .filter(|label| matches!(label.component, AlignmentComponent::Base { .. }))
            .count();
        serde_json::json!({
            "scene_revision": diagnostic_revision_fingerprint(labels.scene_revision.fingerprint()),
            "camera_sequence": labels.camera_sequence,
            "anchors_examined": labels.anchors_examined,
            "labels_projected": labels.labels.len(),
            "base_anchors_examined": labels.base_anchors_examined,
            "base_labels_projected": labels.base_labels_projected,
            "base_labels_presented": presented_base_labels,
            "base_anchors_rejected_by_legibility_or_clipping": labels
                .base_anchors_examined
                .saturating_sub(labels.base_labels_projected),
            "projector_presenter_count_coherent": labels.base_labels_projected == presented_base_labels,
            "read_authority_scene_coherent": authority.scene_revision == labels.scene_revision,
        })
    }

    /// Captures every authority involved in deciding whether vertical scrolling can reuse the
    /// current display derivative. This is intentionally redundant with the high-level presented
    /// state: a target-revision or row-atlas disagreement must remain visible even when the normal
    /// accessor correctly rejects the stale lane.
    pub(crate) fn manual_read_panel_scroll_cache_diagnostic_state(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> serde_json::Value {
        let Some(panel) = self.panels.get(panel_id) else {
            return serde_json::json!({ "panel_present": false });
        };
        let desired_target_revision = self
            .alignment_scene_publication_authority
            .desired_target_revision();
        let committed_target_revision = self.alignment_scene_publication_authority.target_revision;
        let committed_row_target_matches_desired = self
            .alignment_scene_publication_authority
            .committed_row_target_matches_desired(panel_id, track_id);
        let presented_target_revision = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .map(|bundle| bundle.target_revision);
        let current_presented_target_revision = self
            .presented_alignment_lane(panel_id, track_id)
            .map(|presented| presented.target_revision);
        let cache = panel.scroll_display_cache.as_ref();
        let cached = cache.and_then(|cache| {
            cache
                .read_panels
                .iter()
                .find(|read_panel| read_panel.track_id == track_id)
        });
        let Some(cached) = cached else {
            return serde_json::json!({
                "panel_present": true,
                "scroll_cache_present": cache.is_some(),
                "read_panel_cache_present": false,
                "cache_supports_fast_scroll": cache.is_some_and(|cache| cache.supports_fast_scroll),
                "display_dirty": self.display_dirty,
                "desired_target_revision": desired_target_revision,
                "committed_target_revision": committed_target_revision,
                "presented_epoch_target_revision": presented_target_revision,
                "current_presented_target_revision": current_presented_target_revision,
                "committed_row_target_matches_desired": committed_row_target_matches_desired,
            });
        };
        let state = panel.read_panel_scroll_state(track_id);
        let effective_authority = current_read_row_authority(
            cached.presentation_target_revision,
            desired_target_revision,
            committed_row_target_matches_desired,
            cached.row_coordinate_authority,
        );
        let required_materialized_rows = cached
            .total_rows
            .zip(cached.row_origin)
            .zip(cached.row_height)
            .map(|((total_rows, row_origin), row_height)| {
                materialized_alignment_row_range(
                    row_origin,
                    row_height,
                    cached.clip_top,
                    cached.clip_bottom,
                    state.effective_scroll_points(),
                    total_rows,
                    0,
                )
            });
        let materialization_coverage = read_panel_materialization_coverage(
            cached.materialized_row_ranges.as_deref(),
            required_materialized_rows,
        );
        let materialization_covers_required = materialization_coverage.is_covered();
        serde_json::json!({
            "panel_present": true,
            "scroll_cache_present": true,
            "read_panel_cache_present": true,
            "cache_supports_fast_scroll": cache.is_some_and(|cache| cache.supports_fast_scroll),
            "display_dirty": self.display_dirty,
            "desired_target_revision": desired_target_revision,
            "committed_target_revision": committed_target_revision,
            "presented_epoch_target_revision": presented_target_revision,
            "current_presented_target_revision": current_presented_target_revision,
            "committed_row_target_matches_desired": committed_row_target_matches_desired,
            "cache_presentation_target_revision": cached.presentation_target_revision,
            "cache_target_matches_desired": cached.presentation_target_revision == desired_target_revision,
            "cache_target_matches_committed": cached.presentation_target_revision == committed_target_revision,
            "raw_row_coordinate_authority": format!("{:?}", cached.row_coordinate_authority),
            "effective_row_coordinate_authority": format!("{effective_authority:?}"),
            "row_activity_atlas_revision": cached.row_activity_atlas_revision.map(|revision| diagnostic_revision_fingerprint(revision.fingerprint())),
            "materialized_row_ranges": cached.materialized_row_ranges.as_deref(),
            "required_materialized_row_range": required_materialized_rows.map(|(start, end)| serde_json::json!({ "start": start, "end": end })),
            "materialization_coverage": format!("{materialization_coverage:?}"),
            "materialization_covers_required": materialization_covers_required,
            "total_rows": cached.total_rows,
            "row_origin": cached.row_origin,
            "row_height": cached.row_height,
            "density_camera": cached.density_camera,
            "clip_top": cached.clip_top,
            "clip_bottom": cached.clip_bottom,
            "actual_content_bottom": cached.actual_content_bottom,
            "scroll_points": state.scroll_points,
            "effective_scroll_points": state.effective_scroll_points(),
            "content_origin_points": state.content_origin_points,
        })
    }

    pub(crate) fn manual_active_scroll_cache_diagnostic_state(&self) -> serde_json::Value {
        let panel_id = self.panels.active().id;
        serde_json::Value::Array(
            self.alignment_tracks
                .iter()
                .map(|track| {
                    serde_json::json!({
                        "track_id": track.config.id.get(),
                        "authority": self.manual_read_panel_scroll_cache_diagnostic_state(
                            panel_id,
                            track.config.id,
                        ),
                    })
                })
                .collect(),
        )
    }

    fn all_panel_alignment_diagnostic_state(&self) -> Vec<serde_json::Value> {
        let current_target_revision = self
            .alignment_scene_publication_authority
            .desired_target_revision();
        self.panels
            .iter()
            .map(|panel| {
                let tracks = self
                    .alignment_tracks
                    .iter()
                    .map(|track| {
                        let track_id = track.config.id;
                        let state = panel.read_panel_scroll_state(track_id);
                        let cached = panel.scroll_display_cache.as_ref().and_then(|cache| {
                            cache
                                .read_panels
                                .iter()
                                .find(|read_panel| read_panel.track_id == track_id)
                        });
                        let row_authority = cached.map_or(
                            ReadRowCoordinateAuthority::Absent,
                            |cached| {
                                current_read_row_authority(
                                    cached.presentation_target_revision,
                                    current_target_revision,
                                    self.alignment_scene_publication_authority
                                        .committed_row_target_matches_desired(panel.id, track_id),
                                    cached.row_coordinate_authority,
                                )
                            },
                        );
                        let (clip_top, clip_bottom, actual_content_bottom) = cached.map_or(
                            (0.0, 0.0, 0.0),
                            |cached| {
                                (
                                    cached.clip_top,
                                    cached.clip_bottom,
                                    cached.actual_content_bottom,
                                )
                            },
                        );
                        let scroll_maximum = crate::display::read_panel_scrollbar_maximum(
                            state,
                            clip_bottom,
                            row_authority,
                        );
                        let presented = self
                            .presented_alignment_lane(panel.id, track_id)
                            .and_then(|presented| {
                                presented.lane.snapshot.as_ref().map(|snapshot| {
                                    let filter = snapshot.reads().evidence().view_filter();
                                    serde_json::json!({
                                        "target_revision": presented.target_revision,
                                        "scene_revision": diagnostic_revision_fingerprint(snapshot.revision().fingerprint()),
                                        "scene_contracts_current": presented.lane.scene_contracts_current,
                                        "filter_evaluation_fingerprint": filter.fingerprint().get(),
                                        "filter_eligible_records": filter.eligible_records(),
                                        "filter_matching_records": filter.matching_records(),
                                        "row_atlas_filter_evaluation_fingerprint": snapshot.reads().atlas().view_filter().get(),
                                        "summary_evidence_coherent": snapshot.summaries().evidence_revision()
                                            == snapshot.reads().evidence().revision(),
                                        "read_authority": presented.read_authority.is_some(),
                                    })
                                })
                            })
                            .unwrap_or(serde_json::Value::Null);
                        serde_json::json!({
                            "track_id": track_id.get(),
                            "row_authority": format!("{row_authority:?}"),
                            "scroll_cache_authority": self.manual_read_panel_scroll_cache_diagnostic_state(
                                panel.id,
                                track_id,
                            ),
                            "scroll": {
                                "points": state.scroll_points,
                                "effective_points": state.effective_scroll_points(),
                                "content_origin_points": state.content_origin_points,
                                "content_bottom_points": state.content_bottom_points,
                                "latent_fraction": state.latent_scroll_fraction,
                                "maximum_points": scroll_maximum,
                                "clip_top": clip_top,
                                "clip_bottom": clip_bottom,
                                "actual_content_bottom": actual_content_bottom,
                                "density": self.alignment_density_for_panel_track(panel.id, track_id),
                            },
                            "presented": presented,
                            "base_labels": self.alignment_base_label_diagnostic_state(panel.id, track_id),
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({
                    "panel_id": panel.id.get(),
                    "active": panel.id == self.panels.active().id,
                    "contig": panel.contig_name,
                    "start": panel.viewport.interval().start(),
                    "end": panel.viewport.interval().end(),
                    "generation": panel.generation_clock.current().get(),
                    "reference_slice_revision": diagnostic_optional_reference_revision(panel.reference_slice_revision),
                    "read_stack_scroll_points": panel.read_stack_scroll_points,
                    "read_stack_content_bottom_points": panel.read_stack_content_bottom_points,
                    "tracks": tracks,
                })
            })
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    fn manual_diagnostic_state_snapshot(&self) -> serde_json::Value {
        let panel = self.panels.active();
        let tracks = self
            .alignment_tracks
            .iter()
            .map(|track| {
                let projection = track.projection(panel.id);
                let (navigation, staged, telemetry) = projection.map_or_else(
                    || (serde_json::Value::Null, serde_json::Value::Null, serde_json::Value::Null),
                    |projection| {
                        let diagnostic = projection.navigation.diagnostic();
                        let navigation = serde_json::json!({
                            "generation": diagnostic.generation,
                            "request_token": diagnostic.request_token.map(AlignmentSceneRequestToken::get),
                            "phase": format!("{:?}", diagnostic.phase),
                            "owner": format!("{:?}", diagnostic.owner),
                            "products": format!("{:?}", diagnostic.products),
                            "desired_camera": diagnostic.desired_camera.map(|camera| format!("{:?}", camera)),
                            "detail": diagnostic.detail.as_deref(),
                            "invariant_failure": diagnostic.invariant_failure.as_deref(),
                            "journal_entries": diagnostic.journal_entries,
                        });
                        let staged = projection.scene_frame.as_ref().map_or(
                            serde_json::Value::Null,
                            |frame| {
                                serde_json::json!({
                                    "camera_sequence": frame.camera_sequence,
                                    "covered_interval": format!("{:?}", frame.covered_interval),
                                    "covered_rows": { "start": frame.covered_rows.start, "end": frame.covered_rows.end },
                                    "requested_rows": { "start": frame.requested_rows.start, "end": frame.requested_rows.end },
                                    "individual_reads": format!("{:?}", frame.individual_reads),
                                    "vertical_residency": format!("{:?}", frame.vertical_residency),
                                    "scene_contracts_current": frame.scene_contracts_current,
                                    "visible_complete": frame.visible_complete(),
                                    "record_count": frame.snapshot.as_ref().map_or(0, |snapshot| snapshot.reads().evidence().record_count()),
                                    "visible_record_count": frame.row_activity.as_ref().map(|activity| activity.visible_record_count()),
                                    "complete_read_limit": track.config.maximum_alignments,
                                    "atlas_rows": frame.snapshot.as_ref().map_or(0, |snapshot| snapshot.reads().atlas().row_count()),
                                    "read_pages": frame.snapshot.as_ref().map_or(0, |snapshot| snapshot.reads().pages().len()),
                                    "summary_pages": frame.snapshot.as_ref().map_or(0, |snapshot| snapshot.summary_pages().len()),
                                })
                            },
                        );
                        let work = projection.scene_telemetry;
                        let last_pump = work.last_vertical_residency_pump.map(|pump| {
                            serde_json::json!({
                                "checkpoint": pump.checkpoint,
                                "current_scene": format!("{:?}", pump.current_scene),
                                "published_scene": pump.published_scene.map(|scene| format!("{:?}", scene)),
                                "current_rows": { "start": pump.current_prepared_rows.start, "end": pump.current_prepared_rows.end },
                                "target_rows": { "start": pump.target_prepared_rows.start, "end": pump.target_prepared_rows.end },
                                "total_rows": pump.total_rows,
                                "current_read_pages": pump.current_read_pages,
                                "current_summary_pages": pump.current_summary_pages,
                                "target_read_pages": pump.target_read_pages,
                                "target_summary_pages": pump.target_summary_pages,
                                "target_read_page_cache_misses": pump.target_read_page_cache_misses,
                                "prepare_elapsed_nanoseconds": pump.prepare_elapsed_nanoseconds,
                                "pump_elapsed_nanoseconds": pump.pump_elapsed_nanoseconds,
                                "outcome": format!("{:?}", pump.outcome),
                            })
                        });
                        let mut telemetry = serde_json::json!({
                            "camera_updates": work.camera_updates,
                            "camera_only_updates": work.camera_only_updates,
                            "source_work_started": work.source_work_started,
                            "source_checkpoints": work.source_checkpoints,
                            "source_retargets": work.source_retargets,
                            "source_yields": work.source_yields,
                            "source_acquisitions": work.source_acquisitions,
                            "source_refusals": work.source_refusals,
                            "evidence_candidates": work.evidence_candidates,
                            "view_filter_evaluations": work.view_filter_evaluations,
                            "view_filter_records_examined": work.view_filter_records_examined,
                            "view_filter_records_matched": work.view_filter_records_matched,
                            "view_filter_evaluation_nanoseconds": work.view_filter_evaluation_nanoseconds,
                            "last_view_filter_evaluation_nanoseconds": work.last_view_filter_evaluation_nanoseconds,
                            "last_view_filter_contract_fingerprint": work.last_view_filter_contract_fingerprint,
                            "last_view_filter_evaluation_fingerprint": work.last_view_filter_evaluation_fingerprint,
                            "exact_summary_candidates": work.exact_summary_candidates,
                            "row_atlas_candidates": work.row_atlas_candidates,
                            "presentation_candidates": work.presentation_candidates,
                            "scene_publications": work.scene_publications,
                            "foreground_submissions": work.foreground_submissions,
                            "speculation_submissions": work.speculation_submissions,
                            "vertical_checkpoints": work.vertical_residency_checkpoints,
                            "vertical_publications": work.vertical_residency_publications,
                            "vertical_blocks_warmed": work.vertical_residency_blocks_warmed,
                            "vertical_completions": work.vertical_residency_completions,
                            "vertical_budget_limits": work.vertical_residency_budget_limits,
                            "vertical_failures": work.vertical_residency_failures,
                            "vertical_read_page_occurrences": work.vertical_residency_read_page_occurrences,
                            "vertical_read_page_cache_misses": work.vertical_residency_read_page_cache_misses,
                            "vertical_summary_page_occurrences": work.vertical_residency_summary_page_occurrences,
                            "vertical_prepare_nanoseconds": work.vertical_residency_prepare_nanoseconds,
                            "vertical_pump_nanoseconds": work.vertical_residency_pump_nanoseconds,
                            "row_activity_queries": work.row_activity_queries,
                            "row_activity_cache_hits": work.row_activity_cache_hits,
                            "row_activity_cursor_updates": work.row_activity_cursor_updates,
                            "row_activity_rebuilds": work.row_activity_rebuilds,
                            "row_activity_intervals_examined": work.row_activity_intervals_examined,
                            "row_activity_intervals_changed": work.row_activity_intervals_changed,
                            "last_vertical_pump": last_pump,
                        });
                        if let Some(telemetry) = telemetry.as_object_mut() {
                            telemetry.insert(
                                "row_camera_updates_deferred".to_owned(),
                                work.row_camera_updates_deferred.into(),
                            );
                            telemetry.insert(
                                "foreground_row_block_publications".to_owned(),
                                work.foreground_row_block_publications.into(),
                            );
                            telemetry.insert(
                                "return_cache_demotions".to_owned(),
                                work.return_cache_demotions.into(),
                            );
                            telemetry.insert(
                                "return_cache_entries_demoted".to_owned(),
                                work.return_cache_entries_demoted.into(),
                            );
                        }
                        (navigation, staged, telemetry)
                    },
                );
                let scroll = panel.read_panel_scroll_state(track.config.id);
                let (clip_top, clip_bottom, actual_content_bottom) = self
                    .read_panel_metrics(track.config.id)
                    .unwrap_or((0.0, 0.0, 0.0));
                let scroll_maximum = self
                    .read_panel_metrics(track.config.id)
                    .map_or(0.0, |(_, bottom, _)| {
                        self.read_panel_presented_scroll_maximum(track.config.id, bottom)
                    });
                let presented = self
                    .presented_alignment_lane(panel.id, track.config.id)
                    .map_or(serde_json::Value::Null, |presented| {
                        serde_json::json!({
                            "target_revision": presented.target_revision,
                            "source_request_token": presented.source.source_request_token.get(),
                            "desired_request_token": presented.source.desired_request_token.get(),
                            "source_camera": format!("{:?}", presented.source.source_camera),
                            "individual_reads": format!("{:?}", presented.source.individual_reads),
                            "vertical_residency": format!("{:?}", presented.source.vertical_residency),
                            "summary_current": presented.lane.summary_current(),
                            "reads_complete": presented.lane.reads_complete(),
                            "read_authority": presented.read_authority.is_some(),
                            "covered_rows": { "start": presented.lane.covered_rows.start, "end": presented.lane.covered_rows.end },
                            "requested_rows": { "start": presented.lane.requested_rows.start, "end": presented.lane.requested_rows.end },
                        })
                    });
                serde_json::json!({
                    "track_id": track.config.id.get(),
                    "lifecycle": format!("{:?}", track.lifecycle),
                    "navigation": navigation,
                    "staged": staged,
                    "telemetry": telemetry,
                    "presented": presented,
                    "filtering": self.alignment_filter_diagnostic_state(panel.id, track.config.id),
                    "base_labels": self.alignment_base_label_diagnostic_state(panel.id, track.config.id),
                    "row_authority": format!("{:?}", self.read_panel_row_authority(track.config.id)),
                    "scroll_cache_authority": self.manual_read_panel_scroll_cache_diagnostic_state(
                        panel.id,
                        track.config.id,
                    ),
                    "scroll": {
                        "points": scroll.scroll_points,
                        "effective_points": scroll.effective_scroll_points(),
                        "content_origin_points": scroll.content_origin_points,
                        "content_bottom_points": scroll.content_bottom_points,
                        "latent_fraction": scroll.latent_scroll_fraction,
                        "maximum_points": scroll_maximum,
                        "clip_top": clip_top,
                        "clip_bottom": clip_bottom,
                        "actual_content_bottom": actual_content_bottom,
                        "density": self.alignment_density_for_panel_track(panel.id, track.config.id),
                    },
                })
            })
            .collect::<Vec<_>>();
        let panels = self.all_panel_alignment_diagnostic_state();
        let alignment_context_menu = self.alignment_context_menu.as_ref().map_or(
            serde_json::Value::Null,
            |menu| {
                serde_json::json!({
                    "panel_id": menu.target.panel_id.get(),
                    "track_id": menu.target.track_id.get(),
                    "committed_target_revision": menu.target.committed_target_revision,
                    "scene_revision": diagnostic_revision_fingerprint(menu.target.scene_revision.fingerprint()),
                    "genomic_position": menu.target.genomic_position,
                    "hit_present": menu.target.hit.is_some(),
                    "hit_component": menu.target.hit.map(|hit| diagnostic_alignment_component(hit.component)),
                    "target_current": self.alignment_context_target_is_current(menu.target),
                })
            },
        );
        let read_ops = self.alignment_gpu_renderer.operation_counters();
        let summary_ops = self.alignment_summary_gpu_renderer.operation_counters();
        let rectangle_ops = self.rectangle_renderer.operation_counters();
        let context_ops = self.context_scene_gpu_renderer.operation_counters();
        let read_stats = self.alignment_gpu_last_prepare_stats;
        let summary_stats = self.alignment_summary_gpu_last_prepare_stats;
        let budget = self.alignment_scene_budget;
        let ledger_progress = self.alignment_scene_ledger.progress().ok();
        let retained_breakdown = ledger_progress.map_or(serde_json::Value::Null, |progress| {
            let retained = progress.retained;
            serde_json::json!({
                "evidence_bytes": retained.evidence_bytes(),
                "summary_bytes": retained.summary_bytes(),
                "row_atlas_bytes": retained.row_atlas_bytes(),
                "semantic_scene_bytes": retained.semantic_scene_bytes(),
                "gpu_bytes": retained.gpu_bytes(),
                "total_bytes": retained.total(),
            })
        });
        let last_display_invalidation = self
            .manual_diagnostic_trace_runtime
            .last_display_invalidation
            .as_ref()
            .map_or(
                serde_json::Value::Null,
                ManualDiagnosticDisplayInvalidation::json,
            );
        let presented_epoch = self.alignment_presented_epoch.as_ref().map_or(
            serde_json::Value::Null,
            |epoch| {
                epoch.gpu_bundle.as_ref().map_or(serde_json::Value::Null, |bundle| {
                    serde_json::json!({
                        "target_revision": bundle.target_revision,
                        "publication_attempt_sequence": bundle.publication_attempt_sequence,
                        "panel_frames": epoch.panel_frames.len(),
                        "lane_authorities": bundle.lane_authorities.len(),
                        "read_authorities": bundle.read_authorities.len(),
                        "has_read_frame": bundle.reads.is_some(),
                        "has_summary_frame": bundle.summaries.is_some(),
                        "is_current": alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority),
                    })
                })
            },
        );
        serde_json::json!({
            "active_panel": {
                "panel_id": panel.id.get(),
                "contig": panel.contig_name,
                "start": panel.viewport.interval().start(),
                "end": panel.viewport.interval().end(),
                "exact_start": panel.viewport.exact_start(),
                "span": panel.viewport.span(),
                "pixel_width": panel.viewport.pixel_width(),
                "bases_per_pixel": panel.viewport.bases_per_pixel(),
                "generation": panel.generation_clock.current().get(),
                "navigation_phase": format!("{:?}", panel.navigation_presentation.phase),
                "navigation_target_generation": panel.navigation_presentation.target_generation,
                "navigation_target": format!("{:?}", panel.navigation_presentation.target_interval),
                "navigation_notice_visible": panel.navigation_presentation.notice_visible,
                "navigation_invalidation": panel.navigation_presentation.invalidation_reason,
                "committed_content_revision": panel.navigation_presentation.committed_content_revision,
            },
            "workspace": {
                "layout_revision": self.workspace_layout.revision,
                "layout_recomputations": self.workspace_layout_recomputations,
                "genome_overview_recomputations": self.genome_overview_recomputations,
                "navigation_contract_revision": self.navigation_contract_revision,
                "stale_result_rejections": self.navigation_stale_result_rejections,
                "surface_width": self.surface_config.width,
                "surface_height": self.surface_config.height,
                "scale_factor": self.window.scale_factor(),
                "display_dirty": self.display_dirty,
                "display_dirty_mark_sequence": self.manual_diagnostic_trace_runtime.display_dirty_mark_sequence,
                "display_dirty_clear_sequence": self.manual_diagnostic_trace_runtime.display_dirty_clear_sequence,
                "last_display_invalidation": last_display_invalidation,
            },
            "input": {
                "dragging": self.dragging,
                "horizontal_overview_drag": self.horizontal_overview_drag.is_some(),
                "vertical_overview_drag": self.vertical_overview_drag.is_some(),
                "overview_capture": format!("{:?}", self.overview_capture),
                "alignment_context_menu": alignment_context_menu,
            },
            "publication": {
                "committed_target_revision": self.alignment_scene_publication_authority.target_revision,
                "desired_target_revision": self.alignment_scene_publication_authority.desired_target_revision(),
                "attempt_sequence": self.alignment_scene_publication_authority.attempt_sequence(),
                "retry_pending": self.alignment_gpu_publication_retry.is_some(),
                "capacity_reclaim_pending": self.alignment_capacity_reclaim.is_some(),
                "last_budget_pressure": self.alignment_last_gpu_budget_pressure.map(|pressure| format!("{pressure:?}")),
                "presented_epoch": presented_epoch,
            },
            "budget": {
                "budget_bytes": budget.budget_bytes,
                "transition_reserve_bytes": budget.transition_reserve_bytes,
                "transient_budget_bytes": budget.transient_budget_bytes,
                "charged_bytes": budget.charged_bytes,
                "reserved_bytes": budget.reserved_bytes,
                "reserved_transition_bytes": budget.reserved_transition_bytes,
                "transient_bytes": budget.transient_bytes,
                "transient_high_water_bytes": budget.transient_high_water_bytes,
                "pinned_bytes": budget.pinned_bytes,
                "pinned_overage_bytes": budget.pinned_overage_bytes,
                "active_replacement_bundle": budget.active_replacement_bundle,
                "refreshes": budget.refreshes,
                "retained_breakdown": retained_breakdown,
            },
            "prefetch": {
                "reads": self.alignment_gpu_prefetch.reads,
                "summaries": self.alignment_gpu_prefetch.summaries,
                "read_remaining_pages": self.alignment_gpu_prefetch.read_cursor.as_ref().map(AlignmentGpuPrefetchCursor::remaining_pages),
                "summary_remaining_pages": self.alignment_gpu_prefetch.summary_cursor.as_ref().map(AlignmentSummaryGpuPrefetchCursor::remaining_pages),
                "read_retention_limited": self.alignment_gpu_prefetch.read_retention_limited,
                "summary_retention_limited": self.alignment_gpu_prefetch.summary_retention_limited,
                "read_retained_bytes": self.alignment_gpu_prefetch.read_cursor.as_ref().map(AlignmentGpuPrefetchCursor::retained_bytes),
                "summary_retained_bytes": self.alignment_gpu_prefetch.summary_cursor.as_ref().map(AlignmentSummaryGpuPrefetchCursor::retained_bytes),
            },
            "gpu": {
                "read_prepare": {
                    "scenes": read_stats.scenes,
                    "mandatory_pages": read_stats.mandatory_pages,
                    "base_instances": read_stats.base_instances,
                    "overlay_instances": read_stats.overlay_instances,
                    "page_pin_acquisitions": read_stats.page_pin_acquisitions,
                    "geometry_uploaded_bytes": read_stats.geometry_uploaded_bytes,
                    "page_cache_hits": read_stats.page_cache_hits,
                    "page_cache_misses": read_stats.page_cache_misses,
                    "resident_pages": read_stats.resident_pages,
                    "resident_gpu_bytes": read_stats.resident_gpu_bytes,
                },
                "summary_prepare": {
                    "scenes": summary_stats.scenes,
                    "mandatory_pages": summary_stats.mandatory_pages,
                    "depth_instances": summary_stats.depth_instances,
                    "allele_instances": summary_stats.allele_instances,
                    "junction_instances": summary_stats.junction_instances,
                    "modification_instances": summary_stats.modification_instances,
                    "page_pin_acquisitions": summary_stats.page_pin_acquisitions,
                    "geometry_uploaded_bytes": summary_stats.geometry_uploaded_bytes,
                    "page_cache_hits": summary_stats.page_cache_hits,
                    "page_cache_misses": summary_stats.page_cache_misses,
                    "resident_pages": summary_stats.resident_pages,
                    "resident_gpu_bytes": summary_stats.resident_gpu_bytes,
                },
                "read_operations": {
                    "manifest_builds": read_ops.content_manifest_builds,
                    "manifest_pages_enumerated": read_ops.content_manifest_pages_enumerated,
                    "prefetch_keys_inspected": read_ops.prefetch_authority_keys_inspected,
                    "reclaim_keys_inspected": read_ops.reclaim_authority_keys_inspected,
                },
                "summary_operations": {
                    "manifest_builds": summary_ops.content_manifest_builds,
                    "manifest_pages_enumerated": summary_ops.content_manifest_pages_enumerated,
                    "prefetch_keys_inspected": summary_ops.prefetch_authority_keys_inspected,
                    "reclaim_keys_inspected": summary_ops.reclaim_authority_keys_inspected,
                },
                "context_operations": {
                    "content_preparations": context_ops.content_preparations,
                    "camera_capture_attempts": context_ops.camera_capture_attempts,
                    "camera_captures": context_ops.camera_captures,
                    "content_replacement_requests": context_ops.content_replacement_requests,
                    "invalid_camera_captures": context_ops.invalid_camera_captures,
                    "immutable_upload_bytes": context_ops.immutable_upload_bytes,
                    "camera_writes": context_ops.camera_writes,
                },
                "generic_operations": {
                    "preparations": rectangle_ops.preparations,
                    "instances_examined": rectangle_ops.instances_examined,
                    "slab_cache_hits": rectangle_ops.slab_cache_hits,
                    "uploaded_bytes": rectangle_ops.uploaded_bytes,
                    "buffer_reallocations": rectangle_ops.buffer_reallocations,
                },
            },
            "tracks": tracks,
            "panels": panels,
            "data_status": self.data_status,
        })
    }

    fn record_manual_data_status_change(&mut self) {
        if !self.manual_diagnostic_trace_is_recording()
            || self
                .manual_diagnostic_trace_runtime
                .last_observed_data_status
                == self.data_status
        {
            return;
        }
        let previous = std::mem::replace(
            &mut self
                .manual_diagnostic_trace_runtime
                .last_observed_data_status,
            self.data_status.clone(),
        );
        let fields = object_fields(serde_json::json!({
            "previous": previous,
            "current": self.data_status,
        }));
        if manual_trace_status_is_critical(&self.data_status) {
            let message = self.data_status.clone();
            self.freeze_manual_diagnostic_trace(
                "failure",
                "critical_data_status",
                &message,
                fields,
            );
        } else {
            self.manual_diagnostic_trace
                .record("status", "data_status_changed", fields);
        }
    }

    fn save_frozen_manual_diagnostic_trace_if_needed(&mut self) {
        if matches!(
            self.manual_diagnostic_trace.status().state,
            ManualDiagnosticTraceState::Frozen
        ) && self
            .manual_diagnostic_trace_runtime
            .last_saved_path
            .is_none()
        {
            self.save_manual_diagnostic_trace("critical failure");
        }
    }

    fn save_manual_diagnostic_trace(&mut self, reason: &str) {
        let payload = match self.manual_diagnostic_trace.export_payload() {
            Ok(payload) => payload,
            Err(error) => {
                let message = format!("could not serialize manual trace: {error}");
                self.manual_diagnostic_trace_runtime.failure = Some(message.clone());
                tracing::error!(%error, "could not serialize manual diagnostic trace");
                return;
            }
        };
        let directory = self
            .manual_diagnostic_trace_runtime
            .output_directory
            .clone();
        let path = directory.join(&payload.suggested_filename);
        let result = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&directory)?;
            let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
            temporary.write_all(&payload.bytes)?;
            temporary.as_file_mut().sync_all()?;
            temporary.persist(&path).map_err(|error| error.error)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.manual_diagnostic_trace_runtime.last_saved_path = Some(path.clone());
                tracing::info!(
                    %reason,
                    path = %path.display(),
                    media_type = payload.media_type,
                    bytes = payload.bytes.len(),
                    "manual diagnostic trace saved"
                );
            }
            Err(error) => {
                let message = format!("could not save trace to {}: {error}", path.display());
                self.manual_diagnostic_trace_runtime.failure = Some(message.clone());
                tracing::error!(%error, path = %path.display(), "could not save manual diagnostic trace");
            }
        }
    }
}

pub(crate) fn object_fields(value: serde_json::Value) -> ManualDiagnosticTraceFields {
    value.as_object().cloned().unwrap_or_default()
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn manual_trace_status_is_critical(status: &str) -> bool {
    [
        "Could not capture alignment scene",
        "Could not resolve alignment scene lanes",
        "Could not prepare one atomic alignment GPU presentation",
        "Could not capture the current alignment camera presentation",
        "Could not inventory mandatory alignment GPU pages",
        "Could not inventory mandatory summary GPU pages",
        "Could not inspect alignment GPU budget",
        "alignment source backend failed",
        "Read source failed",
    ]
    .iter()
    .any(|needle| status.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_terminal_alignment_statuses_trigger_automatic_freeze() {
        assert!(manual_trace_status_is_critical(
            "Could not capture alignment scene: camera mismatch"
        ));
        assert!(manual_trace_status_is_critical(
            "Complete reads unavailable: alignment source backend failed"
        ));
        assert!(!manual_trace_status_is_critical("Updating view…"));
        assert!(!manual_trace_status_is_critical(
            "Complete alignment presentation is cache-budget limited"
        ));
    }

    #[test]
    fn frame_phase_timings_accumulate_in_their_exact_slot() {
        let mut timings = ManualDiagnosticFrameTimings::default();
        timings.add(
            ManualDiagnosticFramePhase::FirstDisplayRebuild,
            Duration::from_micros(41),
        );
        timings.add(
            ManualDiagnosticFramePhase::FirstDisplayRebuild,
            Duration::from_micros(1),
        );
        timings.add(
            ManualDiagnosticFramePhase::QueueSubmission,
            Duration::from_micros(7),
        );
        assert_eq!(timings.first_display_rebuild_us, 42);
        assert_eq!(timings.queue_submission_us, 7);
        assert_eq!(timings.shell_us, 0);
    }

    #[test]
    fn frame_phase_timings_capture_display_invalidation_boundaries() {
        let mut timings = ManualDiagnosticFrameTimings::default();
        timings.note_display_invalidation_progress(
            ManualDiagnosticFramePhase::FirstDisplayRebuild,
            4,
            2,
        );
        timings.note_display_invalidation_progress(ManualDiagnosticFramePhase::Shell, 7, 3);
        timings.note_display_invalidation_progress(
            ManualDiagnosticFramePhase::SecondDisplayRebuild,
            8,
            4,
        );
        assert_eq!(timings.display_dirty_marks_after_first_rebuild, Some(4));
        assert_eq!(timings.display_dirty_clears_after_first_rebuild, Some(2));
        assert_eq!(timings.display_dirty_marks_after_shell, Some(7));
        assert_eq!(timings.display_dirty_clears_after_shell, Some(3));
        assert_eq!(timings.display_dirty_marks_after_second_rebuild, Some(8));
        assert_eq!(timings.display_dirty_clears_after_second_rebuild, Some(4));
    }

    #[test]
    fn diagnostic_caller_paths_do_not_retain_workspace_prefixes() {
        assert_eq!(
            diagnostic_caller_file(
                "/Users/private/work/rgv/apps/rgv-desktop/src/ui/interaction.rs"
            ),
            "src/ui/interaction.rs"
        );
        assert_eq!(diagnostic_caller_file("generated.rs"), "generated.rs");
    }

    #[test]
    fn diagnostic_predicates_preserve_structure_without_auxiliary_values() {
        let predicate = AlignmentViewPredicate::Auxiliary {
            tag: *b"RG",
            comparison: AlignmentAuxComparison::Equal,
            value: Some(AlignmentAuxFilterValue::String(
                "private-read-group".to_owned(),
            )),
        };
        let diagnostic = diagnostic_alignment_predicate(&predicate);
        let serialized = diagnostic.to_string();
        assert_eq!(diagnostic["kind"], "auxiliary");
        assert_eq!(diagnostic["tag"], "RG");
        assert_eq!(diagnostic["value_kind"], "string");
        assert_eq!(diagnostic["value_redacted"], true);
        assert!(!serialized.contains("private-read-group"));
    }

    #[test]
    fn diagnostic_predicates_keep_reference_scope_and_exact_thresholds() {
        let predicate = AlignmentViewPredicate::Substitution {
            scope: AlignmentPredicateScope::Position {
                contig: "chr7".to_owned(),
                position: 5_525_000,
            },
            observed_base: Some(b'T'),
            minimum_count: Some(2),
            maximum_count: Some(4),
        };
        let diagnostic = diagnostic_alignment_predicate(&predicate);
        assert_eq!(diagnostic["kind"], "substitution");
        assert_eq!(diagnostic["requires_reference"], true);
        assert_eq!(diagnostic["scope"]["kind"], "position");
        assert_eq!(diagnostic["scope"]["position"], 5_525_000);
        assert_eq!(diagnostic["observed_base"], "T");
        assert_eq!(diagnostic["minimum_count"], 2);
        assert_eq!(diagnostic["maximum_count"], 4);
    }
}
