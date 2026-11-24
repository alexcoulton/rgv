//! [`Session`]: the state a browsing session holds and the logic that needs nothing else.
//!
//! The session owns the document, the panels, the track list and arrangement, the reference
//! and track engines, the undo history, the notification log, the status line and the
//! settings. A host (the desktop shell, a headless tool, a test) holds one `Session`, hands it
//! [`Action`]s, and carries out the [`Effect`]s it reports. Alignment sources and their scene
//! engines are still owned by the desktop shell; see
//! `docs/architecture/0009-headless-session.md` for what has and has not moved.

use crate::action::history::{UndoEntry, UndoHistory, track_intents_coalesce_key};
use crate::action::reduce::{self, PanelContext, Reduced};
use crate::action::{Action, ActionOutcome, Effect};
use crate::alignment::track::{
    AlignmentSceneBudgetSnapshot, AlignmentTrackController, EmptyAlignmentState,
    filter_state_from_session,
};
use crate::document_state::{AutosaveMarker, DocumentState};
use crate::gesture::NavigationGestureCommit;
use crate::navigation::ImportedLocusRequest;
use crate::notifications::{NotificationLog, NotificationSeverity};
use crate::panels::{Panel, PanelState, PanelWorkspace};
use crate::settings_writer::SettingsWriter;
use crate::status::{BusyStatus, StatusActivity};
use crate::track_diagnostics::SkippedLineReporter;
use crate::track_ids::TrackIdAllocator;
use anyhow::Result;
use rgv_alignment_scene::AllocationLedger;
use rgv_alignment_scene::ReferenceRevision;
use rgv_core::{
    ContigAliases, DesktopSettings, GenomeAssembly, GenomeAssemblyIdentity, IgvImportReport,
    PanelId, ReferenceDictionary, RegionOfInterest, ReviewItem, SampleMetadataTable,
    SessionAlignmentTrack, SessionFilters, TrackChannel, TrackCollection, TrackConfig, TrackId,
    TrackSource, UnresolvedSessionSource, WorkspacePresentation,
};
use rgv_engine::{ReferenceEngine, TrackEngine, TrackEngineEvent};
use rgv_hts::FastaOpenOptions;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Instant;

/// Most reference and track engines that may be closing at once. A replaced engine is stopped
/// off the calling thread; opening is refused while this many are still shutting down.
pub const MAX_RETIRING_CONTEXT_ENGINES: usize = 8;

/// Session-level state. `P` is the host's panel type; a headless session uses [`PanelState`].
#[allow(clippy::struct_excessive_bools)]
pub struct Session<P: Panel = PanelState> {
    pub panels: PanelWorkspace<P>,
    pub document_state: DocumentState,
    pub regions_of_interest: Vec<RegionOfInterest>,
    pub review_items: Vec<ReviewItem>,
    pub tracks: TrackCollection,
    pub track_presentation: WorkspacePresentation,
    pub suppressed_builtin_gene_bundles: BTreeSet<String>,
    pub track_engine: Option<TrackEngine>,
    pub reference_engine: Option<ReferenceEngine>,
    /// Identity of the currently open FASTA handle. A panel slice is usable only when it carries
    /// this same revision; reopening an equivalent path still creates a distinct authority.
    pub reference_revision: Option<ReferenceRevision>,
    pub reference_open_error: Option<String>,
    /// Splice motifs read from the open FASTA for the junctions Sashimi lanes draw.
    pub splice_motifs: crate::splice_motifs::SpliceMotifCache,
    pub engine_retirement_tasks: Vec<JoinHandle<()>>,
    pub reference_source: Option<TrackSource>,
    pub remote_reference_index_url: Option<String>,
    pub reference_dictionary: Option<ReferenceDictionary>,
    pub contig_aliases: ContigAliases,
    pub assembly: GenomeAssembly,
    pub assembly_identity: Option<GenomeAssemblyIdentity>,
    pub suggested_assembly_identity: Option<GenomeAssemblyIdentity>,
    pub assembly_name: String,
    pub data_status: String,
    pub busy_status: BusyStatus,
    pub settings: DesktopSettings,
    pub settings_path: PathBuf,
    pub settings_writer: SettingsWriter,
    pub workspace_revision: u64,
    pub synthetic: bool,
    pub navigation_contract_revision: u64,
    pub navigation_stale_result_rejections: u64,
    pub current_session_path: Option<PathBuf>,
    pub unresolved_session_sources: Vec<UnresolvedSessionSource>,
    pub session_migration_notices: Vec<String>,
    pub igv_import_report: Option<IgvImportReport>,
    /// An imported IGV locus still waiting for a dictionary to place it.
    pub imported_locus: Option<ImportedLocusRequest>,
    /// An imported feature name to search for once an annotation engine exists.
    pub imported_feature_search: Option<String>,
    pub sample_metadata: Option<SampleMetadataTable>,
    pub cytoband_path: Option<PathBuf>,
    /// Undo and redo steps of the edits applied through the action path.
    pub undo: UndoHistory,
    pub notifications: NotificationLog,
    /// The continuous navigation gesture whose end point is not yet in a panel's history.
    pub navigation_gesture: NavigationGestureCommit,
    pub track_ids: TrackIdAllocator,
    pub last_autosaved: Option<AutosaveMarker>,
    /// Which tracks of the current track engine have already reported skipped lines.
    pub skipped_lines: SkippedLineReporter,
    /// One controller per open alignment track, in workspace order: its scene and coverage
    /// engines, filters and per-panel projections. GPU state derived from them is the host's.
    pub alignment_tracks: Vec<AlignmentTrackController>,
    /// The primary alignment track's settings while no alignment source is open; the first
    /// controller owns them once one is.
    pub empty_alignment: EmptyAlignmentState,
    /// Scene workers being stopped off the calling thread.
    pub alignment_retirement_tasks: Vec<JoinHandle<()>>,
    /// Coverage workers being stopped off the calling thread.
    pub alignment_coverage_retirement_tasks: Vec<JoinHandle<()>>,
    /// The one enforcing authority every alignment source and immutable scene layer is charged
    /// to, sized from `settings.cache_budget_mib` and its memory policy.
    pub alignment_scene_ledger: AllocationLedger,
    pub alignment_scene_budget: AlignmentSceneBudgetSnapshot,
    /// Most alignments any track of the workspace may hold.
    pub workspace_alignment_limit: usize,
    /// Source-owner and request-token identities handed to alignment scene engines.
    pub alignment_scene_identities: crate::alignment::dispatch::AlignmentSceneIdentityClock,
    /// How often, and for how long, the main thread dispatched alignment scene queries.
    pub alignment_dispatch_telemetry: crate::alignment::dispatch::AlignmentDispatchTelemetry,
}

/// What the host's canvas contributes to a panel action: how wide each panel would be, and
/// where track content starts. A window computes these from its layout; a headless host from
/// the image size it will render.
pub struct PanelCanvas<'a> {
    /// Canvas width, in logical points, each panel gets when the workspace holds that many.
    pub width_for: &'a dyn Fn(usize) -> f32,
    /// Top of the track viewport, where a new panel's content starts.
    pub content_top: f32,
}

/// The result of reducing one action inside the session.
#[derive(Debug, Default)]
pub struct SessionReduction {
    pub outcome: ActionOutcome,
    /// Work the host owes, in order.
    pub effects: Vec<Effect>,
    /// Actions that put the document back; empty when the action is not an undo step.
    pub inverse: Vec<Action>,
    /// Identity of the gesture this edit may belong to (see [`UndoEntry::coalesce`]).
    pub coalesce: Option<String>,
}

impl<P: Panel> Session<P> {
    /// A session with one panel and nothing open: no engines, an empty track list, default
    /// arrangement. Hosts that restore a saved workspace fill the public fields afterwards.
    pub fn new(
        initial_panel: P,
        assembly: GenomeAssembly,
        settings: DesktopSettings,
        settings_path: PathBuf,
    ) -> Self {
        let total_cache_bytes = settings.cache_budget_mib.saturating_mul(1024 * 1024).max(1);
        let alignment_scene_ledger = AllocationLedger::with_policy(
            settings.alignment_memory_policy,
            total_cache_bytes,
            total_cache_bytes,
            total_cache_bytes,
        );
        let alignment_scene_budget = alignment_scene_ledger
            .progress()
            .map(|progress| AlignmentSceneBudgetSnapshot::from_progress(progress, 0))
            .unwrap_or_default();
        let filters = SessionFilters::default();
        let (filter_editor, active_filters) =
            filter_state_from_session(&filters).expect("the default alignment filters are valid");
        let workspace_alignment_limit = SessionAlignmentTrack::new(
            TrackId::new(1),
            String::new(),
            TrackSource::Local {
                path: String::new(),
            },
        )
        .maximum_alignments;
        let empty_alignment = EmptyAlignmentState {
            filters,
            filter_editor,
            active_filters,
            name: String::new(),
            visible: true,
            collapsed: false,
            maximum_alignments: workspace_alignment_limit,
        };
        Self {
            panels: PanelWorkspace::new(initial_panel),
            document_state: DocumentState::default(),
            regions_of_interest: Vec::new(),
            review_items: Vec::new(),
            tracks: TrackCollection::default(),
            track_presentation: WorkspacePresentation::default(),
            suppressed_builtin_gene_bundles: BTreeSet::new(),
            track_engine: None,
            reference_engine: None,
            reference_revision: None,
            reference_open_error: None,
            splice_motifs: crate::splice_motifs::SpliceMotifCache::default(),
            engine_retirement_tasks: Vec::new(),
            reference_source: None,
            remote_reference_index_url: None,
            reference_dictionary: None,
            contig_aliases: ContigAliases::default(),
            assembly,
            assembly_identity: None,
            suggested_assembly_identity: None,
            assembly_name: String::new(),
            data_status: String::new(),
            busy_status: BusyStatus::default(),
            settings,
            settings_writer: SettingsWriter::new(settings_path.clone()),
            settings_path,
            workspace_revision: 1,
            synthetic: false,
            navigation_contract_revision: 0,
            navigation_stale_result_rejections: 0,
            current_session_path: None,
            unresolved_session_sources: Vec::new(),
            session_migration_notices: Vec::new(),
            igv_import_report: None,
            imported_locus: None,
            imported_feature_search: None,
            sample_metadata: None,
            cytoband_path: None,
            undo: UndoHistory::default(),
            notifications: NotificationLog::default(),
            navigation_gesture: NavigationGestureCommit::default(),
            track_ids: TrackIdAllocator::default(),
            last_autosaved: None,
            skipped_lines: SkippedLineReporter::default(),
            alignment_tracks: Vec::new(),
            empty_alignment,
            alignment_retirement_tasks: Vec::new(),
            alignment_coverage_retirement_tasks: Vec::new(),
            alignment_scene_ledger,
            alignment_scene_budget,
            workspace_alignment_limit,
            alignment_scene_identities:
                crate::alignment::dispatch::AlignmentSceneIdentityClock::default(),
            alignment_dispatch_telemetry:
                crate::alignment::dispatch::AlignmentDispatchTelemetry::default(),
        }
    }

    // ----- Status line and notifications -----------------------------------------------------

    /// Reports a failure: shown on the status line and kept in the notification log, where a
    /// later progress message cannot overwrite it.
    pub fn report_error(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.notifications
            .push_reported(NotificationSeverity::Error, None, &message);
        self.data_status = message;
    }

    /// Reports something the user should know that is not a failure: shown on the status line
    /// and kept in the notification log as a warning.
    pub fn report_warning(&mut self, message: impl Into<String>) {
        let message = message.into();
        self.notifications
            .push_reported(NotificationSeverity::Warning, None, &message);
        self.data_status = message;
    }

    /// Sets the status line and declares that it describes background work in progress.
    pub fn set_busy_status(&mut self, status: impl Into<String>) {
        self.data_status = status.into();
        self.busy_status.declare(&self.data_status);
    }

    /// Activity of the status line as it currently reads.
    pub fn status_activity(&self) -> StatusActivity {
        self.busy_status.activity_for(&self.data_status)
    }

    // ----- Revisions -------------------------------------------------------------------------

    pub fn bump_workspace_revision(&mut self) -> u64 {
        self.workspace_revision = self.workspace_revision.saturating_add(1);
        self.workspace_revision
    }

    /// The query or presentation contract changed: every panel's navigation transaction must
    /// re-establish that what it shows is still what was asked for.
    pub fn invalidate_navigation_contract(&mut self, reason: &str) {
        self.navigation_contract_revision = self.navigation_contract_revision.saturating_add(1);
        for panel in self.panels.iter_mut() {
            panel
                .state_mut()
                .navigation_presentation
                .invalidate_contract(reason.to_owned());
        }
    }

    // ----- Track queries ---------------------------------------------------------------------

    /// The annotation-pane tracks as the arrangement presents them: in pane order, without
    /// tracks inside a collapsed group, with visibility and preferred height taken from the
    /// arrangement rather than from the track configuration.
    pub fn effective_auxiliary_presentation_configs(&self) -> Vec<TrackConfig> {
        let by_id = self
            .tracks
            .tracks()
            .iter()
            .map(|track| (track.id, track))
            .collect::<BTreeMap<_, _>>();
        self.track_presentation
            .pane_tracks
            .iter()
            .filter(|node| node.channel == TrackChannel::Annotation)
            .filter_map(|node| {
                let mut track = (*by_id.get(&node.track_id)?).clone();
                let parent = self
                    .track_presentation
                    .group_for_track(TrackChannel::Annotation, node.track_id);
                if parent.is_some_and(|group| !group.expanded) {
                    return None;
                }
                track.visible =
                    node.content_visible && parent.is_none_or(|group| group.content_visible);
                track.collapsed = !track.visible;
                if let Some(height) = node.preferred_height {
                    track.height = height;
                }
                Some(track)
            })
            .collect()
    }

    /// The track arrangement as a session document saves it.
    pub fn workspace_presentation_snapshot(&self) -> WorkspacePresentation {
        self.track_presentation.clone()
    }

    // ----- Track engine ----------------------------------------------------------------------

    /// Everything the track engine has reported since the last call, in order. A failure to
    /// receive ends the batch; the events before it are still returned with its message.
    pub fn receive_track_events(&mut self) -> (Vec<TrackEngineEvent>, Option<String>) {
        let mut events = Vec::new();
        let Some(engine) = &self.track_engine else {
            return (events, None);
        };
        loop {
            match engine.try_receive() {
                Ok(Some(event)) => events.push(event),
                Ok(None) => return (events, None),
                Err(error) => return (events, Some(error.to_string())),
            }
        }
    }

    /// Logs a warning the first time each text track of the current engine reports skipped
    /// malformed lines, and the first time its name-search index proves partial. Returns
    /// whether anything was logged.
    pub fn report_track_source_diagnostics(&mut self) -> bool {
        let Some(engine) = &self.track_engine else {
            return false;
        };
        let mut reported = false;
        for (track_id, diagnostics) in engine.source_diagnostics() {
            let name = self
                .tracks
                .tracks()
                .iter()
                .find(|track| track.id == track_id)
                .map_or_else(
                    || format!("track {}", track_id.get()),
                    |track| track.name.clone(),
                );
            if let Some(message) = self.skipped_lines.observe(
                track_id,
                &name,
                diagnostics.malformed_lines,
                diagnostics.first_malformed_line.as_ref(),
            ) {
                tracing::warn!(track_id = track_id.get(), %message, "track source skipped lines");
                self.notifications
                    .push(NotificationSeverity::Warning, Some(track_id), message);
                reported = true;
            }
            if let Some(coverage) = &diagnostics.truncated_search_index
                && let Some(message) = self.skipped_lines.observe_search_truncation(
                    track_id,
                    &name,
                    coverage.complete_through.as_deref(),
                )
            {
                tracing::warn!(
                    track_id = track_id.get(),
                    associations = coverage.associations,
                    %message,
                    "track name-search index is partial"
                );
                self.notifications
                    .push(NotificationSeverity::Warning, Some(track_id), message);
                reported = true;
            }
        }
        reported
    }

    // ----- Reference lifecycle ---------------------------------------------------------------

    /// Starts a reference engine reading `path`. When the source cannot be opened the new
    /// engine is stopped off the calling thread and the error returned; the session's current
    /// reference is untouched either way.
    pub fn start_reference_engine(
        &mut self,
        path: PathBuf,
        options: FastaOpenOptions,
    ) -> Result<ReferenceEngine> {
        let engine = ReferenceEngine::spawn(8);
        if let Err(error) = engine.try_open_with_options(path, options) {
            if let Some(worker) = engine.retire() {
                self.engine_retirement_tasks.push(worker);
            }
            return Err(error.into());
        }
        Ok(engine)
    }

    /// Makes `engine`, reading `source`, the session's reference. The previous engine is
    /// stopped off the calling thread, and the dictionary and revision stay unknown until the
    /// new engine reports them. The host reconnects anything that consumed the old engine's
    /// slices.
    pub fn install_reference_engine(&mut self, engine: ReferenceEngine, source: TrackSource) {
        self.reference_source = Some(source);
        self.reference_dictionary = None;
        self.reference_revision = None;
        self.retire_reference_engine();
        self.reference_engine = Some(engine);
    }

    /// Every panel forgets its reference slice and asks again for its current locus under a
    /// new generation.
    pub fn rerequest_panel_references(&mut self) {
        for panel in self.panels.iter_mut() {
            panel.state_mut().rerequest_reference();
        }
    }

    // ----- Engine lifecycle ------------------------------------------------------------------

    pub fn ensure_engine_retirement_capacity(&mut self, additional: usize) -> Result<()> {
        self.poll_engine_retirement_tasks();
        anyhow::ensure!(
            self.engine_retirement_tasks
                .len()
                .saturating_add(additional)
                <= MAX_RETIRING_CONTEXT_ENGINES,
            "Previous sources are still closing; retry opening after they finish"
        );
        Ok(())
    }

    /// Stops the reference engine off the calling thread. Returns whether one was open.
    pub fn retire_reference_engine(&mut self) -> bool {
        let Some(engine) = self.reference_engine.take() else {
            return false;
        };
        if let Some(worker) = engine.retire() {
            self.engine_retirement_tasks.push(worker);
        }
        true
    }

    /// Stops the track engine off the calling thread. Returns whether one was open.
    pub fn retire_track_engine(&mut self) -> bool {
        // Every track load ends with its engine; a later load of the same id reports afresh.
        self.skipped_lines.reset();
        let Some(engine) = self.track_engine.take() else {
            return false;
        };
        if let Some(worker) = engine.retire() {
            self.engine_retirement_tasks.push(worker);
        }
        true
    }

    pub fn poll_engine_retirement_tasks(&mut self) {
        let mut index = 0;
        while index < self.engine_retirement_tasks.len() {
            if !self.engine_retirement_tasks[index].is_finished() {
                index += 1;
                continue;
            }
            if self.engine_retirement_tasks.remove(index).join().is_err() {
                tracing::warn!("context engine panicked during retirement");
            }
        }
    }

    /// Final shutdown, unlike interactive replacement, owns a blocking join. The host retires
    /// its engines first (it may hold state tied to them) and then calls this.
    pub fn join_engine_retirement_tasks(&mut self) {
        for worker in self.engine_retirement_tasks.drain(..) {
            if worker.join().is_err() {
                tracing::warn!("context engine panicked at shutdown");
            }
        }
    }

    // ----- Settings --------------------------------------------------------------------------

    /// Hands the current settings to the settings worker; rapid saves collapse into one write
    /// of the newest snapshot.
    pub fn submit_settings(&mut self) {
        self.settings_writer.submit(&self.settings);
    }

    /// Surfaces a settings save that failed on the worker. Returns whether one did.
    pub fn poll_settings_writer(&mut self) -> bool {
        let Some(error) = self.settings_writer.take_failure() else {
            return false;
        };
        self.report_warning(format!(
            "Could not save settings to {}: {error}",
            self.settings_path.display()
        ));
        true
    }

    // ----- Actions ---------------------------------------------------------------------------

    /// Whether [`Self::reduce`] carries the action out. The others still need the host: they
    /// start or stop engines the session does not own yet, or re-lay out tracks.
    pub const fn reduces(action: &Action) -> bool {
        reduce::is_panel_action(action) || matches!(action, Action::SetWorkspaceLists { .. })
    }

    /// The panel whose dictionary a panel action is expressed in: a new panel, and a workspace
    /// laid out from a locus list, use the focused panel's; everything else the addressed
    /// panel's.
    pub fn dictionary_panel(&self, action: &Action) -> PanelId {
        let focused = self.panels.active().state().id;
        match action {
            Action::AddPanel { .. } | Action::RestorePanel(_) => focused,
            _ => action.panel_id().unwrap_or(focused),
        }
    }

    /// Carries out one navigation, panel-management or list action and reports the effects the
    /// host owes and the actions that undo it. Nothing is recorded in the undo history.
    ///
    /// `selected_alignment` is the alignment track the [`Self::dictionary_panel`] has selected:
    /// a panel action is expressed in that source's dictionary (or the first alignment
    /// source's), and in the reference dictionary when no alignment source is open.
    pub fn reduce(
        &mut self,
        action: &Action,
        canvas: &PanelCanvas<'_>,
        selected_alignment: Option<TrackId>,
    ) -> Result<SessionReduction> {
        if let Action::SetWorkspaceLists {
            regions,
            review_items,
        } = action
        {
            let (change, inverse) = reduce::reduce_workspace_lists(
                &mut self.regions_of_interest,
                &mut self.review_items,
                regions.clone(),
                review_items.clone(),
            );
            return Ok(SessionReduction {
                outcome: ActionOutcome {
                    changed: inverse.is_some(),
                    ..ActionOutcome::default()
                },
                effects: vec![Effect::WorkspaceListsChanged {
                    regions_changed: change.regions_changed,
                    review_changed: change.review_changed,
                }],
                inverse: inverse.into_iter().collect(),
                coalesce: change
                    .in_place
                    .then(|| "workspace-lists:in-place".to_owned()),
            });
        }
        anyhow::ensure!(
            reduce::is_panel_action(action),
            "{} needs the host",
            action.label()
        );
        let alignment_tracks = &self.alignment_tracks;
        let alignment_dictionary = selected_alignment
            .and_then(|id| {
                alignment_tracks
                    .iter()
                    .find(|track| track.config.id == id)
                    .and_then(|track| track.dictionary.as_ref())
            })
            .or_else(|| {
                alignment_tracks
                    .first()
                    .and_then(|track| track.dictionary.as_ref())
            });
        let ctx = PanelContext {
            dictionary: alignment_dictionary.or(self.reference_dictionary.as_ref()),
            fallback_dictionary: &self.assembly.dictionary,
            aliases: &self.contig_aliases,
            canvas_width_for: canvas.width_for,
            content_top: canvas.content_top,
        };
        let gesture = &mut self.navigation_gesture;
        let settings = &mut self.settings;
        // A still-pending gesture becomes its own history entry and recent locus before a
        // discrete navigation moves the panel, so the gesture's end point is not lost.
        let mut commit_pending_gesture = |panels: &mut PanelWorkspace<P>| {
            if let Some(panel_id) = gesture.take()
                && let Some(locus) = reduce::record_panel_visit(panels, panel_id)
            {
                settings.remember_locus(locus);
            }
        };
        let Reduced {
            outcome,
            effects,
            inverse,
        } = reduce::reduce_panel_action(
            &mut self.panels,
            &ctx,
            action,
            &mut commit_pending_gesture,
        )?;
        Ok(SessionReduction {
            outcome,
            effects,
            inverse,
            coalesce: None,
        })
    }

    /// The undo entry an applied action earned, if it is an undo step.
    pub fn undo_entry(
        action: Action,
        inverse: Vec<Action>,
        coalesce: Option<String>,
    ) -> Option<UndoEntry> {
        (!inverse.is_empty()).then(|| UndoEntry {
            label: action.label(),
            redo: vec![action],
            undo: inverse,
            coalesce,
        })
    }

    /// The gesture a track-arrangement action may be one sample of.
    pub fn track_intents_coalesce_key(action: &Action) -> Option<String> {
        match action {
            Action::ApplyTrackIntents { intents } => track_intents_coalesce_key(intents),
            _ => None,
        }
    }
}

/// Something that can carry out actions and keeps an undo history of them: the desktop shell,
/// or a bare [`Session`]. [`apply_action`], [`undo_action`] and [`redo_action`] are the one
/// implementation of recording and replaying edits, whoever the host is.
pub trait ActionHost {
    fn undo_history(&mut self) -> &mut UndoHistory;

    /// Applies an action and returns the undo entry it earned, without recording it.
    fn perform_action(&mut self, action: Action) -> Result<(ActionOutcome, Option<UndoEntry>)>;
}

/// How an undo or redo request ended.
#[derive(Debug)]
pub enum HistoryStep {
    /// There was nothing to undo or redo.
    Nothing,
    /// The step was applied.
    Done { label: &'static str },
    /// The document moved under the entry (a source vanished, the window is too narrow for the
    /// panel). The step is dropped; it is never left half applied on the stack.
    Failed {
        label: &'static str,
        error: anyhow::Error,
    },
}

/// Applies one user intent and records how to undo it.
pub fn apply_action<H: ActionHost>(host: &mut H, action: Action) -> Result<ActionOutcome> {
    let (outcome, entry) = host.perform_action(action)?;
    if let Some(entry) = entry {
        host.undo_history().record(entry, Instant::now());
    }
    Ok(outcome)
}

/// Takes back the most recent recorded edit.
pub fn undo_action<H: ActionHost>(host: &mut H) -> HistoryStep {
    let Some(entry) = host.undo_history().take_undo() else {
        return HistoryStep::Nothing;
    };
    for action in entry.undo.clone() {
        if let Err(error) = host.perform_action(action) {
            return HistoryStep::Failed {
                label: entry.label,
                error,
            };
        }
    }
    let label = entry.label;
    host.undo_history().push_redo(entry);
    HistoryStep::Done { label }
}

/// Re-applies the most recently undone edit.
pub fn redo_action<H: ActionHost>(host: &mut H) -> HistoryStep {
    let Some(entry) = host.undo_history().take_redo() else {
        return HistoryStep::Nothing;
    };
    let mut undo = Vec::new();
    for action in entry.redo.clone() {
        match host.perform_action(action) {
            Ok((_, Some(mut redone))) => {
                // The fresh inverse describes the document as it is now.
                redone.undo.append(&mut undo);
                undo = redone.undo;
            }
            Ok((_, None)) => {}
            Err(error) => {
                return HistoryStep::Failed {
                    label: entry.label,
                    error,
                };
            }
        }
    }
    let label = entry.label;
    if !undo.is_empty() {
        host.undo_history().record_redone(UndoEntry {
            label,
            redo: entry.redo,
            undo,
            coalesce: None,
        });
    }
    HistoryStep::Done { label }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgv_core::{
        ContigId, GenomicInterval, PresentationNodeId, TrackId, TrackIntent, TrackKind, Viewport,
    };

    fn headless_session() -> Session {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000_000)]);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 10_000).unwrap();
        let panel = PanelState::new(
            PanelId::new(1),
            Viewport::new(interval, 600.0).unwrap(),
            None,
            "chr1".to_owned(),
            1_000_000,
        );
        Session::new(
            panel,
            GenomeAssembly::from_dictionary("test", dictionary),
            DesktopSettings::default(),
            PathBuf::from("settings.json"),
        )
    }

    fn feature_track(id: u64, name: &str) -> TrackConfig {
        TrackConfig::new(
            TrackId::new(id),
            name.to_owned(),
            TrackKind::Feature,
            TrackSource::Local {
                path: format!("{name}.bed"),
            },
        )
    }

    #[test]
    fn a_new_reference_makes_every_panel_ask_again_at_its_locus() {
        let mut session = headless_session();
        let generation_before = session.panels.active().generation_clock.current();
        {
            let panel = session.panels.active_mut();
            panel.reference_slice_generation = Some(3);
            panel.reference_settled_generation = Some(3);
            panel.reference_query_error_generation = Some(2);
            panel.pending_queries.reference = true;
        }
        session.rerequest_panel_references();
        let panel = session.panels.active();
        assert_eq!(panel.requested_locus.as_deref(), Some("chr1:1-10000"));
        assert!(panel.generation_clock.current() > generation_before);
        assert!(panel.reference_slice.is_none() && panel.reference_slice_generation.is_none());
        assert!(panel.reference_settled_generation.is_none());
        assert!(panel.reference_query_error_generation.is_none());
        assert!(!panel.pending_queries.reference);
    }

    #[test]
    fn effective_auxiliary_configs_follow_the_arrangement_not_the_track_configs() {
        let mut session = headless_session();
        let (genes, peaks) = (feature_track(3, "genes"), feature_track(4, "peaks"));
        session.tracks.push(genes.clone());
        session.tracks.push(peaks.clone());
        session.track_presentation = WorkspacePresentation::from_legacy(&[], &[genes, peaks]);
        let node = |id| PresentationNodeId::Track {
            channel: TrackChannel::Annotation,
            track_id: TrackId::new(id),
        };
        session
            .track_presentation
            .apply_intents(&[
                TrackIntent::SetNodeVisibility {
                    target: node(4),
                    visible: false,
                },
                TrackIntent::MoveNode {
                    target: node(4),
                    before: Some(node(3)),
                },
            ])
            .unwrap();

        let effective = session.effective_auxiliary_presentation_configs();
        let order = effective.iter().map(|track| track.id).collect::<Vec<_>>();
        assert_eq!(order, [TrackId::new(4), TrackId::new(3)]);
        assert!(!effective[0].visible && effective[0].collapsed);
        assert!(effective[1].visible && !effective[1].collapsed);
        // The configs in the track list keep their own flags; only the projection changes.
        assert!(session.tracks.tracks().iter().all(|track| track.visible));
        assert_eq!(
            session.workspace_presentation_snapshot(),
            session.track_presentation
        );
    }
}
