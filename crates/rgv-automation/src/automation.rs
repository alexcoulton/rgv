use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use rgv_core::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentPredicateExpression,
    AlignmentSortMode, AlignmentViewPredicate, CoverageScaleTransform, DetailLevel,
    ModifiedBaseColorMode, OverlappingMatePolicy, PanelId, ReadBaseDisplayMode, ReadStackingMode,
    ReviewStatus, TrackId, TrackKind, TrackSource,
};

pub const AUTOMATION_SCHEMA_VERSION: u32 = 6;
pub const MINIMUM_AUTOMATION_SCHEMA_VERSION: u32 = 1;
pub const MAXIMUM_AUTOMATION_COMMANDS: usize = 10_000;
pub const MAXIMUM_AUTOMATION_STRING_BYTES: usize = 16_384;
pub const MAXIMUM_AUTOMATION_OPTION_BYTES: usize = 64 * 1024;
pub const MAXIMUM_AUTOMATION_LOCI: usize = 16;
pub const MAXIMUM_AUTOMATION_TRACK_SELECTION: usize = 1_024;
pub const MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS: usize = 1_024;
pub const MAXIMUM_AUTOMATION_EVIDENCE_FIELDS: usize = 128;
pub const MAXIMUM_AUTOMATION_ALIGNMENT_RESULTS: usize = 128;
pub const MAXIMUM_AUTOMATION_HISTORY_ENTRIES: usize = 256;
pub const MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS: u64 = 100;
pub const MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS: u64 = 600_000;
/// Most features one `query_features` page may return.
pub const MAXIMUM_AUTOMATION_FEATURE_RESULTS: usize = 500;
/// Widest interval, in bases, one `query_features` command may ask about (10 Mb).
pub const MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN: u64 = 10_000_000;
/// Largest `query_features` paging offset.
pub const MAXIMUM_AUTOMATION_FEATURE_OFFSET: usize = 1_000_000;
/// Longest `goto_gene` query, in bytes; the same bound the interactive annotation search uses.
pub const MAXIMUM_AUTOMATION_GENE_QUERY_BYTES: usize = 256;
/// Largest `goto_gene` flank, in bases, added on each side of the matched feature (10 Mb).
pub const MAXIMUM_AUTOMATION_GENE_FLANK_BASES: u64 = 10_000_000;
pub use rgv_core::MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationBatch {
    pub schema_version: u32,
    pub commands: Vec<AutomationCommand>,
}

impl AutomationBatch {
    pub fn from_json(json: &str) -> Result<Self, AutomationError> {
        let batch: Self = serde_json::from_str(json)?;
        batch.validate()?;
        Ok(batch)
    }

    pub fn read(path: impl AsRef<Path>) -> Result<Self, AutomationError> {
        let path = path.as_ref();
        let mut batch = Self::from_json(&std::fs::read_to_string(path)?)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        batch.resolve_paths(std::path::absolute(parent)?);
        Ok(batch)
    }

    pub fn to_json(&self) -> Result<String, AutomationError> {
        self.validate()?;
        let mut redacted = self.clone();
        for command in &mut redacted.commands {
            command.redact_sources();
        }
        Ok(format!("{}\n", serde_json::to_string_pretty(&redacted)?))
    }

    /// Resolves local inputs and outputs against the directory containing a
    /// batch file. A local command service can call the same method with its
    /// explicitly selected working root.
    pub fn resolve_paths(&mut self, root: impl AsRef<Path>) {
        let root = root.as_ref();
        for command in &mut self.commands {
            command.resolve_paths(root);
        }
    }

    /// Resolves local paths against an explicitly selected control root and
    /// rejects every path, read or written, that lies outside it.
    ///
    /// Containment is decided on the real filesystem location: `..` segments
    /// are folded lexically and then symbolic links in the deepest existing
    /// ancestor are resolved, so neither `../x` nor a link inside the root
    /// that points elsewhere can reach outside. A not-yet-existing output is
    /// judged by its nearest existing directory; a dangling link is refused.
    /// The stored paths remain the lexical ones under the root as given.
    pub fn resolve_paths_within(&mut self, root: impl AsRef<Path>) -> Result<(), AutomationError> {
        let root = normalize_automation_path(&std::path::absolute(root)?);
        let real_root = real_automation_path(&root)?;
        let roots = ControlRoot {
            lexical: &root,
            real: &real_root,
        };
        for command in &mut self.commands {
            command.resolve_paths_within(&roots)?;
        }
        Ok(())
    }

    /// Refuses remote sources a control client may not make the viewer fetch
    /// (see [`check_automation_remote_url`]). Local sources are covered by
    /// [`Self::resolve_paths_within`].
    pub fn check_remote_sources(&self, allow_private_network: bool) -> Result<(), AutomationError> {
        for command in &self.commands {
            if let AutomationCommand::SetGenome {
                source: TrackSource::Remote { url },
                ..
            }
            | AutomationCommand::LoadTrack {
                source: TrackSource::Remote { url },
                ..
            } = command
            {
                check_automation_remote_url(url, allow_private_network)?;
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), AutomationError> {
        if !(MINIMUM_AUTOMATION_SCHEMA_VERSION..=AUTOMATION_SCHEMA_VERSION)
            .contains(&self.schema_version)
        {
            return Err(AutomationError::UnsupportedVersion(self.schema_version));
        }
        if self.commands.is_empty() || self.commands.len() > MAXIMUM_AUTOMATION_COMMANDS {
            return Err(AutomationError::InvalidCommandCount(self.commands.len()));
        }
        for (index, command) in self.commands.iter().enumerate() {
            command
                .validate()
                .map_err(|reason| AutomationError::InvalidCommand { index, reason })?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AutomationCommand {
    /// Return the complete workspace snapshot (genome, panels, tracks, layout, selection). Read-only.
    DescribeWorkspace,
    /// Return the retained evidence snapshot of the active panel's selection, or `null` when
    /// nothing is selected. Read-only.
    DescribeSelection,
    /// Return the input-to-present timeline of the most recent vertical-scrollbar gesture, or
    /// `null` when none was recorded. Read-only.
    DescribeInteractionTrace,
    /// Return the status of the interaction recording (qualification tooling). Read-only.
    DescribeInteractionRecording,
    /// Start recording native input events to a file (qualification tooling). Fails with
    /// `invalid_state` in a build without diagnostics.
    StartInteractionRecording {
        /// Output file for the recording. A relative path is resolved against the batch file's
        /// directory or, through the control service, against the control root; a path outside
        /// the control root is rejected.
        path: String,
        /// Optional free-text fixture label stored in the recording header (non-empty, at most
        /// 16384 bytes).
        #[serde(default)]
        fixture: Option<String>,
        /// When false (the default), key presses that produce text are not recorded verbatim;
        /// when true, typed text is kept in the recording.
        #[serde(default)]
        record_text: bool,
    },
    /// Stop and finalise the interaction recording started by `start_interaction_recording`.
    StopInteractionRecording,
    /// Open a native-input observation scope inside the active interaction recording
    /// (qualification tooling).
    BeginNativeObservation {
        /// Caller-chosen identifier of the stress run (non-empty, at most 16384 bytes).
        run_id: String,
        /// Caller-chosen identifier of the step within the run (non-empty, at most 16384 bytes).
        step_id: String,
        /// The window region the observed input is expected to land on. Omit for no
        /// expectation.
        #[serde(default)]
        expected_target: Option<crate::interaction_stress::StressTarget>,
    },
    /// Close the native-input observation scope and return its summary.
    EndNativeObservation,
    /// Write a labelled workspace checkpoint into the active interaction recording.
    CheckpointInteractionRecording {
        /// Checkpoint label (non-empty, at most 16384 bytes).
        label: String,
    },
    /// Start the manual diagnostic trace. Fails with `invalid_state` in a build without
    /// diagnostics.
    StartDiagnosticTrace,
    /// Stop the manual diagnostic trace and save it; reports the event counts and saved path.
    StopDiagnosticTrace,
    /// Clear the active panel's selection and retained evidence.
    ClearSelection,
    /// Replace the active panel's retained evidence with a new evidence revision captured from
    /// the current query. Fails with `no_selection` when nothing is selected. For an
    /// `exact_base` selection whose one-base evidence is not available it first fits the
    /// panel's viewport to that single base, which visibly changes the view.
    RefreshSelection,
    /// Return the review queue (ID, label, locus, status per item). Read-only.
    DescribeReviewQueue,
    /// Return one panel's navigation history without moving its cursor. Read-only.
    DescribeNavigationHistory {
        /// Positive stable ID of the panel whose history to describe.
        panel_id: PanelId,
    },
    /// Discard every track and panel, keeping only the active genome and the active panel's
    /// locus. Destructive: unsaved workspace changes are lost without confirmation.
    NewWorkspace,
    /// Replace the visible workspace with a saved RGV project file.
    OpenWorkspace {
        /// Project file to open. A relative path is resolved against the batch file's directory
        /// or, through the control service, against the control root; a path outside the
        /// control root is rejected.
        path: String,
        /// Must be true; otherwise the command fails with
        /// `workspace_replacement_not_confirmed` and changes nothing.
        confirm_replace: bool,
        /// What to do when the current workspace has unsaved changes (default `reject`).
        #[serde(default)]
        unsaved_changes: AutomationUnsavedChangesPolicy,
        /// Time allowed for the project to load, in milliseconds, 100 to 600000 (default
        /// 30000).
        #[serde(default = "default_settle_timeout_millis")]
        #[cfg_attr(feature = "schema", schemars(range(min = 100, max = 600_000)))]
        timeout_millis: u64,
    },
    /// Open a reference FASTA as the workspace genome. Only a local source is supported; a
    /// remote source fails with `unsupported_command`.
    SetGenome {
        /// The reference FASTA to open. Must be a local file.
        source: TrackSource,
        /// Optional assembly display name (non-empty, at most 16384 bytes).
        #[serde(default)]
        name: Option<String>,
    },
    /// Load one track and wait up to 30 seconds for its source to open. The result reports the
    /// new stable track ID.
    LoadTrack {
        /// Where to read the track from. A remote (HTTP/HTTPS) source is supported for
        /// alignment tracks only; remote reference and annotation sources fail with
        /// `unsupported_command`.
        source: TrackSource,
        /// Track kind. Omit to infer it from the source's file name; an unrecognised name is
        /// loaded as `Feature`.
        #[serde(default)]
        kind: Option<TrackKind>,
        /// Optional display name for the track (non-empty, at most 16384 bytes).
        #[serde(default)]
        name: Option<String>,
    },
    /// Remove one track from the workspace. Outstanding alignment handles expire and retained
    /// evidence is marked stale.
    RemoveTrack {
        /// Positive stable ID of the track to remove.
        track_id: TrackId,
    },
    /// Navigate to one or more loci. A single locus retargets only the active panel and leaves
    /// the other panels alone; two or more loci replace the whole panel arrangement with one
    /// panel per locus, in order.
    Goto {
        /// One to 16 locus strings, each a single whitespace-free token. Coordinates are
        /// one-based and inclusive at both ends: `chr1:1,000,001-1,100,000` (thousands
        /// separators optional), `chr1:12345` (that base with 20 bases of flank on each side),
        /// or a bare contig name for the whole contig. Gene symbols are not resolved.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 16)))]
        loci: Vec<String>,
    },
    /// Pan, zoom, or fit one panel's viewport. The panel becomes the active panel and the
    /// result reports the exact resulting interval.
    ChangeViewport {
        /// Positive stable ID of the panel to move.
        panel_id: PanelId,
        /// The pan, zoom, or fit to apply. Numeric positions are zero-based; intervals are
        /// half-open `[start, end)`.
        operation: AutomationViewportOperation,
    },
    /// Append a new panel showing a locus.
    AddPanel {
        /// Locus string with one-based inclusive coordinates, in the forms accepted by `goto`
        /// (for example `chr1:1,000,001-1,100,000`).
        locus: String,
    },
    /// Close one panel. Fails with `invalid_state` when the panel does not exist or is the only
    /// remaining panel.
    ClosePanel {
        /// Positive stable ID of the panel to close.
        panel_id: PanelId,
    },
    /// Legacy untyped option. Prefer `configure`, whose variants are validated by type.
    SetOption {
        /// What the option applies to: the workspace, one track, or one panel.
        scope: AutomationOptionScope,
        /// Option name (non-empty, at most 16384 bytes). Workspace keys: `show_reference`,
        /// `show_coverage`, `show_reads`, `show_junctions`, `show_junction_labels`,
        /// `show_coverage_center_line`, `view_as_pairs`, `quality_weighted_alleles`,
        /// `shade_read_bases_by_quality`, `show_modified_bases`, `show_modification_frequency`,
        /// `minimum_mapping_quality`, `include_secondary`, `include_supplementary`,
        /// `minimum_base_quality`, `minimum_allele_fraction`, `vertical_density`,
        /// `coverage_scale_maximum`, `coverage_scale_transform`, `minimum_indel_size`,
        /// `minimum_event_label_size`. Track keys: `visible`, `collapsed`, `height`, `name`,
        /// `sample_key`. Panel keys: `vertical_scroll_points`, `annotation_scroll_points`. An
        /// unknown key fails with `invalid_command`.
        key: String,
        /// The option value as JSON (boolean, number, string, or null, depending on the key);
        /// at most 65536 bytes when serialised.
        value: serde_json::Value,
    },
    /// Apply one typed setting. The result reports `requested`, `effective`, `changed`, and
    /// `clamped`.
    Configure {
        /// The setting to apply, selected by its `setting` discriminator.
        setting: AutomationSetting,
    },
    /// Group the reads of alignment tracks into labelled row blocks.
    Group {
        /// Positive stable IDs of the alignment tracks to group; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
        /// Grouping mode. Ignored when `tag` is supplied.
        mode: AlignmentGroupMode,
        /// Order of the resulting groups (default `natural`).
        #[serde(default)]
        direction: AlignmentOrderDirection,
        /// Two-character SAM auxiliary tag to group by, e.g. `HP` (a letter followed by a letter
        /// or digit). When supplied it overrides `mode` with tag grouping.
        #[serde(default)]
        #[cfg_attr(
            feature = "schema",
            schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))
        )]
        tag: Option<String>,
        /// Zero-based genomic position on the active panel's contig that `base` and `insertion`
        /// grouping examine. Only valid with those modes and without `tag`; defaults to the
        /// centre of the active panel's viewport.
        #[serde(default)]
        anchor_position: Option<u64>,
    },
    /// Sort the reads of alignment tracks within their groups.
    Sort {
        /// Positive stable IDs of the alignment tracks to sort; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
        /// Sort key. Ignored when `tag` is supplied.
        mode: AlignmentSortMode,
        /// Sort direction (default `natural`).
        #[serde(default)]
        direction: AlignmentOrderDirection,
        /// Two-character SAM auxiliary tag to sort by, e.g. `HP` (a letter followed by a letter
        /// or digit). When supplied it overrides `mode` with tag sorting.
        #[serde(default)]
        #[cfg_attr(
            feature = "schema",
            schemars(regex(pattern = r"^[A-Za-z][A-Za-z0-9]$"))
        )]
        tag: Option<String>,
        /// Zero-based genomic position on the active panel's contig whose read base `base`
        /// sorting compares. Only valid with that mode and without `tag`; defaults to the
        /// centre of the active panel's viewport.
        #[serde(default)]
        anchor_position: Option<u64>,
    },
    /// Replace the auxiliary-tag filter of alignment tracks. This changes the alignment query:
    /// outstanding alignment handles expire and retained evidence is marked stale.
    Filter {
        /// Positive stable IDs of the alignment tracks to filter; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
        /// Semicolon-separated SAM-tag predicates combined with logical AND, at most 16384
        /// bytes: a bare tag (`HP`) requires the tag to exist; `TAG=value`, `TAG!=value`,
        /// `TAG>value`, `TAG>=value`, `TAG<value`, `TAG<=value` compare its value (for example
        /// `HP=1;NM<=2`). An empty string clears the filter.
        expression: String,
    },
    /// Select a track, a panel, or the evidence at one genomic position.
    Select {
        /// What to select.
        selection: AutomationSelection,
    },
    /// Find alignment records in one track of one panel and return opaque handles for them.
    /// The search covers the interval the committed alignment scene has prepared (reported as
    /// zero-based half-open `query_start`/`query_end`), which can be wider than the viewport.
    /// Every call invalidates the handles returned by earlier calls. Fails with `too_dense`
    /// when individual reads are not available at the current zoom.
    FindAlignments {
        /// Positive stable ID of the panel to search in.
        panel_id: PanelId,
        /// Positive stable ID of the alignment track to search.
        track_id: TrackId,
        /// Which records to find.
        selector: AutomationAlignmentSelector,
        /// Maximum number of hits to return, 1 to 128 (default 25). `truncated` reports
        /// whether more records matched.
        #[serde(default = "default_alignment_result_limit")]
        #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 128)))]
        maximum_results: usize,
    },
    /// Select the alignment record identified by a handle from `find_alignments`. Fails with
    /// `stale_handle` when the handle is unknown or its query has changed.
    SelectAlignment {
        /// Opaque alignment handle returned by `find_alignments` (non-empty, at most 16384
        /// bytes). It expires when the backing query changes or `find_alignments` runs again.
        handle: String,
        /// Viewport change applied with the selection (default `none`).
        #[serde(default)]
        reveal: AutomationAlignmentReveal,
        /// If set, the command fails with `workspace_revision_conflict` and changes nothing
        /// unless the viewer's current `workspace_revision` equals this value.
        #[serde(default)]
        expected_workspace_revision: Option<u64>,
    },
    /// Describe the selected alignment's mate availability and supplementary-chain segments
    /// (zero-based half-open `start`/`end` per segment). Read-only.
    DescribeRelatedAlignments,
    /// Navigate to the selected alignment's mate or to one segment of its supplementary chain.
    NavigateRelatedAlignment {
        /// Which related record to open.
        relationship: AutomationAlignmentRelationship,
        /// Zero-based index into `supplementary_segments` as reported by
        /// `describe_related_alignments`. Required for `supplementary_segment`; must be omitted
        /// for `mate`.
        #[serde(default)]
        segment_index: Option<usize>,
        /// Where to show the related record (default `current_panel`).
        #[serde(default)]
        destination: AutomationNavigationDestination,
        /// If set, the command fails with `workspace_revision_conflict` and changes nothing
        /// unless the viewer's current `workspace_revision` equals this value.
        #[serde(default)]
        expected_workspace_revision: Option<u64>,
    },
    /// Navigate the active panel to a review item's locus.
    NavigateReviewItem {
        /// Which review item to open.
        action: AutomationReviewNavigation,
    },
    /// Move one panel through its navigation history. The panel becomes the active panel.
    NavigateHistory {
        /// Positive stable ID of the panel whose history to traverse.
        panel_id: PanelId,
        /// Step back, step forward, or jump to an entry.
        action: AutomationHistoryNavigation,
    },
    /// List the annotation features (genes, transcripts, BED intervals, peaks) that overlap one
    /// genomic interval. Read-only and independent of every panel: it reads the loaded feature
    /// and gene-model tracks directly, so it does not navigate, select, or change the workspace,
    /// and the interval need not be shown by any panel. All coordinates are zero-based and
    /// half-open `[start, end)`. Results are ordered by track (workspace order), then `start`,
    /// `end`, and `name`, so `offset`/`limit` paging is stable. Fails with `resource_not_found`
    /// when no feature track is loaded or a named track or the contig does not exist,
    /// `too_dense` when a track holds more records in the interval than one query may read
    /// (ask for a narrower interval), and `timeout` when the tracks do not answer in time.
    QueryFeatures {
        /// Contig (reference sequence) name as the workspace reports it, e.g. `chr1`; non-empty,
        /// at most 16384 bytes. Aliases the viewer accepts in a locus (`1` for `chr1`) are
        /// resolved when a reference dictionary is loaded.
        contig: String,
        /// Zero-based inclusive start of the interval, in bases (one less than the one-based
        /// coordinate in a locus string). 0 to `end` - 1.
        start: u64,
        /// Zero-based exclusive end of the interval, in bases (half-open `[start, end)`; equals
        /// the one-based inclusive end). Must be greater than `start` and at most `start` +
        /// 10000000 (a 10 Mb span). Clamped to the contig length when that is known.
        end: u64,
        /// Positive stable IDs of the feature or gene-model tracks to read (`tracks[].id` in
        /// the workspace snapshot); 1 to 1024 entries. Omit to read every loaded feature and
        /// gene-model track. A track of another kind fails with `invalid_command`.
        #[serde(default)]
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Option<Vec<TrackId>>,
        /// Number of matching features to skip, for paging; 0 to 1000000 (default 0).
        #[serde(default)]
        #[cfg_attr(feature = "schema", schemars(range(min = 0, max = 1_000_000)))]
        offset: usize,
        /// Maximum number of features to return, 1 to 500 (default 100). `truncated` reports
        /// whether matching features remain beyond this page.
        #[serde(default = "default_feature_result_limit")]
        #[cfg_attr(feature = "schema", schemars(range(min = 1, max = 500)))]
        limit: usize,
    },
    /// Find one gene or other named annotation feature by name or identifier and navigate one
    /// panel to it. The search covers every loaded searchable annotation track and is
    /// case-insensitive. The command navigates only when the match is unambiguous: exactly one
    /// feature matches the whole query, or the query matches nothing exactly and exactly one
    /// feature matches it by prefix. It fails with `resource_not_found` when nothing matches or
    /// no searchable track is loaded, `ambiguous_match` when several features match and none is
    /// preferred (nothing is navigated), and `timeout` when the search does not answer in time.
    /// The result reports the zero-based half-open interval the panel now shows. An interactive
    /// annotation search still running in the window is superseded by this command.
    GotoGene {
        /// Gene symbol, feature name, or feature identifier to search for, e.g. `BRCA1`;
        /// non-empty, at most 256 bytes. Matching ignores case and surrounding whitespace.
        query: String,
        /// Positive stable ID of the panel to navigate (`panels[].id` in the workspace
        /// snapshot). Omit to navigate the focused panel. Other panels are not moved and the
        /// focused panel does not change.
        #[serde(default)]
        panel_id: Option<PanelId>,
        /// Bases (base pairs) of context added on each side of the matched feature, 0 to
        /// 10000000 (default 0). The padded interval is clamped to the contig.
        #[serde(default)]
        #[cfg_attr(feature = "schema", schemars(range(min = 0, max = 10_000_000)))]
        flank_bases: u64,
    },
    /// Wait until every pending query and render has settled, so later commands observe the
    /// final state. Fails with `timeout` when the deadline passes.
    WaitUntilSettled {
        /// Maximum time to wait, in milliseconds, 100 to 600000 (default 30000).
        #[serde(default = "default_settle_timeout_millis")]
        #[cfg_attr(feature = "schema", schemars(range(min = 100, max = 600_000)))]
        timeout_millis: u64,
    },
    /// Write an image of the current view to a file.
    Snapshot {
        /// Output image file. A relative path is resolved against the batch file's directory
        /// or, through the control service, against the control root; a path outside the
        /// control root is rejected.
        path: String,
        /// Which part of the window to capture (default `visible_workspace`).
        #[serde(default)]
        scope: AutomationSnapshotScope,
        /// Image format (default `png`). `native_window` supports `png` only.
        #[serde(default)]
        format: AutomationSnapshotFormat,
        /// PNG pixels per logical point, from 0.25 to 8. The default 1.0 gives one pixel per
        /// point. Ignored for SVG; must be 1.0 for the `native_window` scope, which is always
        /// captured at the window's own pixel density.
        #[serde(
            default = "default_snapshot_scale",
            skip_serializing_if = "is_default_snapshot_scale"
        )]
        #[cfg_attr(feature = "schema", schemars(range(min = 0.25, max = 8.0)))]
        scale: f32,
    },
    /// Write a structured text export of evidence in the active panel's current interval. The
    /// tabular alignment, pileup, modification, junction, variant, and quantitative exports
    /// state their convention in a `# coordinates` header (`start` is one-based and `end` is
    /// one-based inclusive), are capped at 64 MiB, and end with an `# export_truncated` line
    /// when the cap is reached.
    Export {
        /// Output file. A relative path is resolved against the batch file's directory or,
        /// through the control service, against the control root; a path outside the control
        /// root is rejected.
        path: String,
        /// Which evidence to export.
        kind: AutomationExportKind,
    },
    /// Save the workspace as an RGV project file.
    Save {
        /// Output project file. A relative path is resolved against the batch file's directory
        /// or, through the control service, against the control root; a path outside the
        /// control root is rejected.
        path: String,
    },
    /// Quit the application once the batch report has been written.
    Exit,
}

impl AutomationCommand {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::DescribeWorkspace => "describe_workspace",
            Self::DescribeSelection => "describe_selection",
            Self::DescribeInteractionTrace => "describe_interaction_trace",
            Self::DescribeInteractionRecording => "describe_interaction_recording",
            Self::StartInteractionRecording { .. } => "start_interaction_recording",
            Self::StopInteractionRecording => "stop_interaction_recording",
            Self::BeginNativeObservation { .. } => "begin_native_observation",
            Self::EndNativeObservation => "end_native_observation",
            Self::CheckpointInteractionRecording { .. } => "checkpoint_interaction_recording",
            Self::StartDiagnosticTrace => "start_diagnostic_trace",
            Self::StopDiagnosticTrace => "stop_diagnostic_trace",
            Self::ClearSelection => "clear_selection",
            Self::RefreshSelection => "refresh_selection",
            Self::DescribeReviewQueue => "describe_review_queue",
            Self::DescribeNavigationHistory { .. } => "describe_navigation_history",
            Self::NewWorkspace => "new_workspace",
            Self::OpenWorkspace { .. } => "open_workspace",
            Self::SetGenome { .. } => "set_genome",
            Self::LoadTrack { .. } => "load_track",
            Self::RemoveTrack { .. } => "remove_track",
            Self::Goto { .. } => "goto",
            Self::ChangeViewport { .. } => "change_viewport",
            Self::AddPanel { .. } => "add_panel",
            Self::ClosePanel { .. } => "close_panel",
            Self::SetOption { .. } => "set_option",
            Self::Configure { .. } => "configure",
            Self::Group { .. } => "group",
            Self::Sort { .. } => "sort",
            Self::Filter { .. } => "filter",
            Self::Select { .. } => "select",
            Self::FindAlignments { .. } => "find_alignments",
            Self::SelectAlignment { .. } => "select_alignment",
            Self::DescribeRelatedAlignments => "describe_related_alignments",
            Self::NavigateRelatedAlignment { .. } => "navigate_related_alignment",
            Self::NavigateReviewItem { .. } => "navigate_review_item",
            Self::NavigateHistory { .. } => "navigate_history",
            Self::QueryFeatures { .. } => "query_features",
            Self::GotoGene { .. } => "goto_gene",
            Self::WaitUntilSettled { .. } => "wait_until_settled",
            Self::Snapshot { .. } => "snapshot",
            Self::Export { .. } => "export",
            Self::Save { .. } => "save",
            Self::Exit => "exit",
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::DescribeWorkspace
            | Self::DescribeSelection
            | Self::DescribeInteractionTrace
            | Self::DescribeInteractionRecording
            | Self::StopInteractionRecording
            | Self::EndNativeObservation
            | Self::StartDiagnosticTrace
            | Self::StopDiagnosticTrace
            | Self::ClearSelection
            | Self::RefreshSelection
            | Self::DescribeReviewQueue
            | Self::DescribeRelatedAlignments
            | Self::NewWorkspace
            | Self::Exit => Ok(()),
            Self::StartInteractionRecording { path, fixture, .. } => {
                validate_string(path, "interaction recording path")?;
                validate_optional_string(fixture.as_deref(), "fixture")
            }
            Self::BeginNativeObservation {
                run_id,
                step_id,
                expected_target,
            } => {
                validate_string(run_id, "run_id")?;
                validate_string(step_id, "step_id")?;
                if let Some(target) = expected_target {
                    target.validate().map_err(|error| error.to_string())?;
                }
                Ok(())
            }
            Self::CheckpointInteractionRecording { label } => {
                validate_string(label, "recording checkpoint label")
            }
            Self::DescribeNavigationHistory { panel_id }
            | Self::NavigateHistory { panel_id, .. } => validate_panel_id(*panel_id),
            Self::OpenWorkspace {
                path,
                timeout_millis,
                ..
            } => {
                validate_string(path, "workspace path")?;
                validate_settle_timeout(*timeout_millis)
            }
            Self::SetGenome { source, name } | Self::LoadTrack { source, name, .. } => {
                validate_source(source)?;
                validate_optional_string(name.as_deref(), "name")
            }
            Self::RemoveTrack { track_id } => validate_track_id(*track_id),
            Self::Goto { loci } => {
                if loci.is_empty() || loci.len() > MAXIMUM_AUTOMATION_LOCI {
                    return Err(format!(
                        "loci must contain 1..={MAXIMUM_AUTOMATION_LOCI} entries"
                    ));
                }
                for locus in loci {
                    validate_string(locus, "locus")?;
                }
                Ok(())
            }
            Self::AddPanel { locus } => validate_string(locus, "locus"),
            Self::ChangeViewport {
                panel_id,
                operation,
            } => {
                if panel_id.get() == 0 {
                    return Err("panel_id must be positive".to_owned());
                }
                operation.validate()
            }
            Self::ClosePanel { panel_id } => {
                if panel_id.get() == 0 {
                    Err("panel_id must be positive".to_owned())
                } else {
                    Ok(())
                }
            }
            Self::SetOption { scope, key, value } => {
                scope.validate()?;
                validate_string(key, "option key")?;
                let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
                if bytes.len() > MAXIMUM_AUTOMATION_OPTION_BYTES {
                    return Err(format!(
                        "option value exceeds {MAXIMUM_AUTOMATION_OPTION_BYTES} bytes"
                    ));
                }
                Ok(())
            }
            Self::Configure { setting } => setting.validate(),
            Self::Group {
                track_ids,
                mode,
                tag,
                anchor_position,
                ..
            } => {
                validate_track_selection(track_ids)?;
                validate_optional_sam_tag(tag.as_deref())?;
                if anchor_position.is_some()
                    && (tag.is_some()
                        || !matches!(
                            mode,
                            AlignmentGroupMode::Base | AlignmentGroupMode::Insertion
                        ))
                {
                    return Err(
                        "anchor_position is only valid for base or insertion grouping".to_owned(),
                    );
                }
                Ok(())
            }
            Self::Sort {
                track_ids,
                mode,
                tag,
                anchor_position,
                ..
            } => {
                validate_track_selection(track_ids)?;
                validate_optional_sam_tag(tag.as_deref())?;
                if anchor_position.is_some() && (tag.is_some() || *mode != AlignmentSortMode::Base)
                {
                    return Err("anchor_position is only valid for base sorting".to_owned());
                }
                Ok(())
            }
            Self::Filter {
                track_ids,
                expression,
            } => {
                validate_track_selection(track_ids)?;
                if expression.len() <= MAXIMUM_AUTOMATION_STRING_BYTES {
                    Ok(())
                } else {
                    Err(format!(
                        "filter expression must be no more than {MAXIMUM_AUTOMATION_STRING_BYTES} bytes"
                    ))
                }
            }
            Self::Select { selection } => selection.validate(),
            Self::FindAlignments {
                panel_id,
                track_id,
                selector,
                maximum_results,
            } => {
                validate_panel_id(*panel_id)?;
                validate_track_id(*track_id)?;
                selector.validate()?;
                if !(1..=MAXIMUM_AUTOMATION_ALIGNMENT_RESULTS).contains(maximum_results) {
                    return Err(format!(
                        "maximum_results must be between 1 and {MAXIMUM_AUTOMATION_ALIGNMENT_RESULTS}"
                    ));
                }
                Ok(())
            }
            Self::SelectAlignment { handle, .. } => validate_string(handle, "alignment handle"),
            Self::NavigateRelatedAlignment {
                relationship,
                segment_index,
                ..
            } => match relationship {
                AutomationAlignmentRelationship::Mate if segment_index.is_some() => {
                    Err("segment_index is only valid for supplementary_segment".to_owned())
                }
                AutomationAlignmentRelationship::SupplementarySegment
                    if segment_index.is_none() =>
                {
                    Err("segment_index is required for supplementary_segment".to_owned())
                }
                _ => Ok(()),
            },
            Self::NavigateReviewItem { action } => action.validate(),
            Self::QueryFeatures {
                contig,
                start,
                end,
                track_ids,
                offset,
                limit,
            } => {
                validate_string(contig, "contig")?;
                if start >= end {
                    return Err(
                        "end must be greater than start (zero-based, half-open interval)"
                            .to_owned(),
                    );
                }
                if end - start > MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN {
                    return Err(format!(
                        "the interval must span no more than {MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN} bases"
                    ));
                }
                if let Some(track_ids) = track_ids {
                    validate_track_selection(track_ids)?;
                }
                if *offset > MAXIMUM_AUTOMATION_FEATURE_OFFSET {
                    return Err(format!(
                        "offset must be no greater than {MAXIMUM_AUTOMATION_FEATURE_OFFSET}"
                    ));
                }
                if !(1..=MAXIMUM_AUTOMATION_FEATURE_RESULTS).contains(limit) {
                    return Err(format!(
                        "limit must be between 1 and {MAXIMUM_AUTOMATION_FEATURE_RESULTS}"
                    ));
                }
                Ok(())
            }
            Self::GotoGene {
                query,
                panel_id,
                flank_bases,
            } => {
                if query.trim().is_empty() || query.len() > MAXIMUM_AUTOMATION_GENE_QUERY_BYTES {
                    return Err(format!(
                        "query must be non-empty and no more than {MAXIMUM_AUTOMATION_GENE_QUERY_BYTES} bytes"
                    ));
                }
                if let Some(panel_id) = panel_id {
                    validate_panel_id(*panel_id)?;
                }
                if *flank_bases > MAXIMUM_AUTOMATION_GENE_FLANK_BASES {
                    return Err(format!(
                        "flank_bases must be no greater than {MAXIMUM_AUTOMATION_GENE_FLANK_BASES}"
                    ));
                }
                Ok(())
            }
            Self::WaitUntilSettled { timeout_millis } => validate_settle_timeout(*timeout_millis),
            Self::Snapshot {
                path, scope, scale, ..
            } => {
                validate_string(path, "output path")?;
                validate_snapshot_scale(*scale, *scope)
            }
            Self::Export { path, .. } | Self::Save { path } => validate_string(path, "output path"),
        }
    }

    fn redact_sources(&mut self) {
        match self {
            Self::SetGenome { source, .. } | Self::LoadTrack { source, .. } => {
                *source = source.redacted_for_persistence();
            }
            Self::FindAlignments {
                selector: AutomationAlignmentSelector::ExactReadName { read_name },
                ..
            } => "[redacted]".clone_into(read_name),
            _ => {}
        }
    }

    fn resolve_paths(&mut self, root: &Path) {
        match self {
            Self::SetGenome { source, .. } | Self::LoadTrack { source, .. } => {
                if let TrackSource::Local { path } = source {
                    let source_path = Path::new(path);
                    if source_path.is_relative() {
                        *path = normalize_automation_path(&root.join(source_path))
                            .display()
                            .to_string();
                    }
                }
            }
            Self::OpenWorkspace { path, .. }
            | Self::StartInteractionRecording { path, .. }
            | Self::Snapshot { path, .. }
            | Self::Export { path, .. }
            | Self::Save { path } => {
                let output = PathBuf::from(&*path);
                if output.is_relative() {
                    *path = normalize_automation_path(&root.join(output))
                        .display()
                        .to_string();
                }
            }
            _ => {}
        }
    }

    fn resolve_paths_within(&mut self, root: &ControlRoot<'_>) -> Result<(), AutomationError> {
        let resolve = |value: &mut String| -> Result<(), AutomationError> {
            *value = root.contain(value)?.display().to_string();
            Ok(())
        };
        match self {
            Self::SetGenome { source, .. } | Self::LoadTrack { source, .. } => {
                if let TrackSource::Local { path } = source {
                    resolve(path)?;
                }
            }
            Self::OpenWorkspace { path, .. }
            | Self::StartInteractionRecording { path, .. }
            | Self::Snapshot { path, .. }
            | Self::Export { path, .. }
            | Self::Save { path } => {
                resolve(path)?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// A deterministic mutation of one existing panel's genomic camera.
///
/// Zoom factors are expressed in user-facing magnification: values greater
/// than one zoom in and values below one zoom out.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum AutomationViewportOperation {
    /// Pan by a signed number of bases.
    PanBases {
        /// Signed pan distance in bases (base pairs). Positive moves the view toward higher
        /// coordinates, negative toward lower. Must not be zero; the view is clamped to the
        /// contig.
        delta: i64,
    },
    /// Pan by a signed fraction of the visible span.
    PanFraction {
        /// Signed pan distance as a fraction of the currently visible span (1.0 = one full view
        /// width). Positive moves toward higher coordinates. Must not be zero; -100 to 100. The
        /// view is clamped to the contig.
        #[cfg_attr(feature = "schema", schemars(range(min = -100.0, max = 100.0)))]
        fraction: f64,
    },
    /// Zoom in or out around an anchor.
    Zoom {
        /// Magnification factor, 0.01 to 100: above 1 zooms in (the visible span is divided by
        /// the factor), below 1 zooms out.
        #[cfg_attr(feature = "schema", schemars(range(min = 0.01, max = 100.0)))]
        factor: f64,
        /// Zero-based genomic position on the panel's current contig that keeps its screen
        /// position while zooming. Omit to zoom around the viewport centre.
        #[serde(default)]
        anchor_position: Option<u64>,
    },
    /// Show exactly one zero-based, half-open interval `[start, end)` of the panel's current
    /// contig.
    FitInterval {
        /// Zero-based inclusive start of the interval, in bases, on the panel's current contig.
        /// Clamped to the last base of the contig.
        start: u64,
        /// Zero-based exclusive end of the interval (half-open `[start, end)`); must be greater
        /// than `start`. Equals the one-based inclusive end. Clamped to the contig length.
        end: u64,
    },
}

impl AutomationViewportOperation {
    fn validate(self) -> Result<(), String> {
        match self {
            Self::PanBases { delta } if delta != 0 => Ok(()),
            Self::PanBases { .. } => Err("pan delta must not be zero".to_owned()),
            Self::PanFraction { fraction }
                if fraction.is_finite()
                    && fraction != 0.0
                    && (-100.0..=100.0).contains(&fraction) =>
            {
                Ok(())
            }
            Self::PanFraction { .. } => {
                Err("pan fraction must be finite, non-zero, and between -100 and 100".to_owned())
            }
            Self::Zoom { factor, .. } if factor.is_finite() && (0.01..=100.0).contains(&factor) => {
                Ok(())
            }
            Self::Zoom { .. } => {
                Err("zoom factor must be finite and between 0.01 and 100".to_owned())
            }
            Self::FitInterval { start, end } if start < end => Ok(()),
            Self::FitInterval { .. } => {
                Err("fit interval must have a start smaller than its end".to_owned())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationUnsavedChangesPolicy {
    /// Fail with `unsaved_changes` and leave the workspace untouched.
    #[default]
    Reject,
    /// Save the current workspace to its existing project file first, then open. Fails with
    /// `unsaved_changes` when the workspace has never been saved.
    SaveThenOpen,
    /// Drop the unsaved changes and open.
    Discard,
}

/// Typed configuration accepted by automation schema v4. Each variant owns
/// its valid scope, preventing key/scope mismatches at the protocol boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum AutomationSetting {
    /// Request a window client-area size. The window system may not honour it exactly.
    WindowSize {
        /// Window client-area width in logical points (not physical pixels), 640 to 3840.
        #[cfg_attr(feature = "schema", schemars(range(min = 640, max = 3840)))]
        width: u32,
        /// Window client-area height in logical points (not physical pixels), 400 to 2160.
        #[cfg_attr(feature = "schema", schemars(range(min = 400, max = 2160)))]
        height: u32,
    },
    /// Set the interface zoom of the running window.
    InterfaceScalePercent {
        /// Interface zoom in percent, 75 to 200; 100 is the unscaled interface.
        #[cfg_attr(feature = "schema", schemars(range(min = 75, max = 200)))]
        value: u16,
    },
    /// Set how far beyond the viewport reads are decoded ahead of panning. Saved to the user's
    /// desktop settings, so it outlives the workspace; a change re-queries every panel.
    ReadBrowsingBufferPercent {
        /// Decoded-read margin on each side of the viewport, as a percentage of the viewport
        /// span, 0 to 400 (default 100).
        #[cfg_attr(feature = "schema", schemars(range(min = 0, max = 400)))]
        value: u16,
    },
    /// Set the total cache budget. Saved to the user's desktop settings, so it outlives the
    /// workspace; a change re-queries every panel.
    TotalCacheBudgetMib {
        /// Cache budget in mebibytes (MiB), 128 to 4096 (application default 512).
        #[cfg_attr(feature = "schema", schemars(range(min = 128, max = 4096)))]
        value: usize,
    },
    /// Choose the pipeline that draws auxiliary tracks. Saved to the user's desktop settings, so
    /// it outlives the workspace. `tiles` is experimental and covers only the track kinds the
    /// viewer has opted in (see the user guide); every other track keeps the legacy pipeline.
    TrackPipeline {
        /// `legacy` (the default) or `tiles`.
        value: rgv_core::TrackPipeline,
    },
    /// Expand or collapse the Properties panel.
    PropertiesPanelExpanded {
        /// True expands the Properties panel, false collapses it.
        value: bool,
    },
    /// Set the width of the Properties panel.
    PropertiesPanelWidth {
        /// Properties panel width in logical points, 280 to 520.
        #[cfg_attr(feature = "schema", schemars(range(min = 280.0, max = 520.0)))]
        value: f32,
    },
    /// Transient control signal matching a held Properties resize sash. This
    /// gesture state is never persisted in a project.
    PropertiesPanelResizeActive {
        /// True marks the sash as held, deferring the resolution-dependent query refresh; false
        /// releases it and schedules that refresh once.
        value: bool,
    },
    /// Show or hide the reference sequence. Stored in the first (primary) alignment track's
    /// display settings.
    ShowReference {
        /// True shows the reference sequence, false hides it.
        value: bool,
    },
    /// Show or hide alignment coverage. Stored in the first (primary) alignment track's display
    /// settings.
    ShowCoverage {
        /// True shows alignment coverage, false hides it.
        value: bool,
    },
    /// Show or hide the reads row of one alignment track: the track named by `track_id`, or
    /// the first (primary) alignment track when `track_id` is omitted. Other alignment tracks
    /// are not changed.
    ShowReads {
        /// True shows the track's reads, false hides them.
        value: bool,
        /// Positive stable ID of the alignment track to change, as reported in the workspace
        /// snapshot. Omitted or `null` addresses the first (primary) alignment track.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track_id: Option<TrackId>,
    },
    /// Show or hide splice-junction arcs. Stored in the first (primary) alignment track's
    /// display settings.
    ShowJunctions {
        /// True shows splice-junction arcs, false hides them.
        value: bool,
    },
    /// Draw paired reads as linked templates. Stored in the first (primary) alignment track's
    /// display settings.
    ViewAsPairs {
        /// True draws the two mates of a pair as one linked template, false draws each read
        /// separately.
        value: bool,
    },
    /// Choose how coverage counts bases covered by both mates of one template. Stored in the
    /// first (primary) alignment track's settings; a change re-queries every panel, expires
    /// alignment handles, and marks retained evidence stale.
    OverlappingMatePolicy {
        /// The counting policy to apply.
        value: OverlappingMatePolicy,
    },
    /// Set how reads are coloured on the listed alignment tracks.
    AlignmentColorMode {
        /// Positive stable IDs of the alignment tracks to change; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
        /// The read colouring mode to apply to the listed tracks.
        value: AlignmentColorMode,
    },
    /// Replace the view filter of the listed alignment tracks: records that do not match are
    /// removed from the reads, coverage, and exports. A change re-queries every panel, expires
    /// alignment handles, and marks retained evidence stale. A `substitution` predicate
    /// requires a loaded reference.
    AlignmentViewFilter {
        /// Positive stable IDs of the alignment tracks to change; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
        /// The predicates a record must satisfy. An expression with an empty `all` list clears
        /// the filter.
        value: AlignmentPredicateExpression,
    },
    /// Replace the highlight filter of the listed alignment tracks: matching records are
    /// highlighted and nothing is removed. A `substitution` predicate requires a loaded
    /// reference.
    AlignmentHighlightFilter {
        /// Positive stable IDs of the alignment tracks to change; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
        /// The predicates a record must satisfy to be highlighted. An expression with an empty
        /// `all` list clears the highlight.
        value: AlignmentPredicateExpression,
    },
    /// Choose whether the listed alignment tracks keep reads on stable rows or repack them for
    /// every query.
    ReadStackingMode {
        /// Positive stable IDs of the alignment tracks to change; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
        /// The stacking mode to apply to the listed tracks.
        value: ReadStackingMode,
    },
    /// Set the largest number of records for which the listed alignment tracks prepare
    /// complete individual reads. The workspace-wide limit is raised to at least this value; a
    /// change re-queries every panel, expires alignment handles, and marks retained evidence
    /// stale.
    CompleteReadLimit {
        /// Positive stable IDs of the alignment tracks to change; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
        /// Maximum number of alignment records, 1000 to 10000000.
        #[cfg_attr(feature = "schema", schemars(range(min = 1000, max = 10_000_000)))]
        value: usize,
    },
    /// One-shot stable-baseline replacement. This is deliberately distinct
    /// from the persistent compact stacking mode. Every listed track must use `stable` read
    /// stacking. Also accepted under the legacy name `compact_repack_reads`.
    #[serde(alias = "compact_repack_reads")]
    RebuildStableReadArchitecture {
        /// Positive stable ID of the panel whose row assignment is rebuilt.
        panel_id: PanelId,
        /// Positive stable IDs of the alignment tracks to rebuild; 1 to 1024 entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 1, max = 1024)))]
        track_ids: Vec<TrackId>,
    },
    /// Set the default read-row density of one alignment track and clear that track's
    /// per-panel density overrides: the track named by `track_id`, or the first (primary)
    /// alignment track when `track_id` is omitted.
    AlignmentDensity {
        /// Positive stable ID of the alignment track to change, as reported in the workspace
        /// snapshot. Omitted or `null` addresses the first (primary) alignment track.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track_id: Option<TrackId>,
        /// Read-row height multiplier: a row is 7 logical points tall at 1.0 and larger values
        /// draw taller rows. Batch validation accepts 0.25 to 4, but the viewer rejects values
        /// below 0.35 with `setting_out_of_range`, so the usable range is 0.35 to 4.
        #[cfg_attr(feature = "schema", schemars(range(min = 0.25, max = 4.0)))]
        value: f32,
    },
    /// Include or exclude secondary alignments. Stored in the first (primary) alignment track's
    /// settings; a change re-queries every panel, expires alignment handles, and marks retained
    /// evidence stale.
    IncludeSecondary {
        /// True includes secondary alignments (SAM flag 0x100), false excludes them.
        value: bool,
    },
    /// Include or exclude supplementary alignments. Stored in the first (primary) alignment
    /// track's settings; a change re-queries every panel, expires alignment handles, and marks
    /// retained evidence stale.
    IncludeSupplementary {
        /// True includes supplementary alignments (SAM flag 0x800), false excludes them.
        value: bool,
    },
    /// Set the vertical scale of coverage depth. Stored in the first (primary) alignment
    /// track's display settings.
    CoverageScaleTransform {
        /// The scale transform to apply.
        value: CoverageScaleTransform,
    },
    /// Choose which read bases are drawn. Stored in the first (primary) alignment track's
    /// display settings.
    ReadBaseDisplayMode {
        /// The base display mode to apply.
        value: ReadBaseDisplayMode,
    },
    /// Shade read bases by base quality. Stored in the first (primary) alignment track's
    /// display settings.
    ShadeReadBasesByQuality {
        /// True shades read bases by their base quality, false draws them at full strength.
        value: bool,
    },
    /// Show or hide base-modification calls. Stored in the first (primary) alignment track's
    /// display settings.
    ShowModifiedBases {
        /// True draws base-modification (MM/ML) calls, false hides them.
        value: bool,
    },
    /// Choose how base-modification calls are coloured. Stored in the first (primary) alignment
    /// track's display settings.
    ModifiedBaseColorMode {
        /// The colouring mode to apply.
        value: ModifiedBaseColorMode,
    },
    /// Expand or collapse a whole presentation section without changing its members' own
    /// visibility, grouping, or heights.
    PresentationSectionExpanded {
        /// The section to expand or collapse.
        channel: rgv_core::TrackChannel,
        /// True expands the section, false collapses it.
        value: bool,
    },
    /// Show or hide the content of the reference lane or of one track row.
    PresentationNodeVisible {
        /// The node to show or hide: `reference` or a `track`. A `group` is rejected; use
        /// `presentation_group_visible`.
        target: rgv_core::PresentationNodeId,
        /// True shows the node's content, false hides it.
        value: bool,
    },
    /// Set the preferred height of the reference lane or of one track row.
    PresentationNodeHeight {
        /// The node to resize: `reference` or a `track`. A `group` is rejected.
        target: rgv_core::PresentationNodeId,
        /// Preferred height in logical points: 26 to 1200 for the reference lane and
        /// annotation-channel tracks, 40 to 1200 for reads-channel tracks.
        #[cfg_attr(feature = "schema", schemars(range(min = 26.0, max = 1200.0)))]
        value: f32,
    },
    /// Expand or collapse a presentation group's member rows.
    PresentationGroupExpanded {
        /// The group to expand or collapse; must be a `group` node.
        target: rgv_core::PresentationNodeId,
        /// True shows the group's member rows, false collapses them.
        value: bool,
    },
    /// Show or hide the content of a presentation group.
    PresentationGroupVisible {
        /// The group to show or hide; must be a `group` node.
        target: rgv_core::PresentationNodeId,
        /// True shows the group's content, false hides it.
        value: bool,
    },
    /// Show or hide a composite (for example a quantitative overlay) without changing its
    /// members' own visibility.
    PresentationCompositeVisible {
        /// Channel that owns the composite. Must be `annotation`; composites do not exist in
        /// the reads channel.
        channel: rgv_core::TrackChannel,
        /// Positive composite ID as reported in `presentation_composites` of the workspace
        /// snapshot.
        #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
        composite_id: u32,
        /// True shows the composite, false hides it.
        value: bool,
    },
    /// Place tracks of one channel in a new presentation group.
    PresentationGroup {
        /// Channel in which to create the group.
        channel: rgv_core::TrackChannel,
        /// Positive stable IDs of the tracks to group; 2 to 1024 distinct entries.
        #[cfg_attr(feature = "schema", schemars(length(min = 2, max = 1024)))]
        track_ids: Vec<TrackId>,
    },
    /// Dissolve a presentation group, returning its members to the top level.
    PresentationUngroup {
        /// The group to dissolve; must be a `group` node.
        target: rgv_core::PresentationNodeId,
    },
    /// Set the height of the reads pane.
    ReadPaneHeight {
        /// Height of the reads pane in logical points, 100 to 1200.
        #[cfg_attr(feature = "schema", schemars(range(min = 100.0, max = 1200.0)))]
        value: f32,
    },
    /// Set the preferred height of a track's annotation-channel row.
    TrackHeight {
        /// Positive stable ID of the track, as reported in the workspace snapshot
        /// (`tracks[].id`).
        track_id: TrackId,
        /// Track height in logical points, 40 to 1200.
        #[cfg_attr(feature = "schema", schemars(range(min = 40.0, max = 1200.0)))]
        value: f32,
    },
    /// Hide or show the content of a track's annotation-channel row. `value: true` is the same
    /// operation as `track_visible` with `value: false`.
    TrackCollapsed {
        /// Positive stable ID of the track, as reported in the workspace snapshot
        /// (`tracks[].id`).
        track_id: TrackId,
        /// True hides the row's content, false shows it.
        value: bool,
    },
    /// Show or hide the content of a track's annotation-channel row.
    TrackVisible {
        /// Positive stable ID of the track, as reported in the workspace snapshot
        /// (`tracks[].id`).
        track_id: TrackId,
        /// True shows the row's content, false hides it.
        value: bool,
    },
    /// Scroll one alignment track's read panel in one genomic panel: the track named by
    /// `track_id`, or the first (primary) alignment track when `track_id` is omitted.
    VerticalScrollPoints {
        /// Positive stable ID of the alignment track whose read panel to scroll, as reported
        /// in the workspace snapshot. Omitted or `null` addresses the first (primary) alignment
        /// track.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        track_id: Option<TrackId>,
        /// Positive stable ID of the panel, as reported in the workspace snapshot
        /// (`panels[].id`).
        panel_id: PanelId,
        /// Vertical scroll offset in logical points from the top (0 = top), non-negative.
        /// Clamped to the read panel's maximum; the result's `effective` gives the applied
        /// value.
        #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
        value: f32,
    },
    /// Scroll the annotation pane. The annotation camera is workspace-wide, so the offset
    /// applies to every panel.
    AnnotationScrollPoints {
        /// Positive stable ID of the panel whose scrollbar issues the request. It must exist
        /// but does not make the offset panel-local.
        panel_id: PanelId,
        /// Vertical scroll offset of the annotation pane in logical points from the top (0 =
        /// top), non-negative. Clamped to `maximum_annotation_scroll_points`.
        #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
        value: f32,
    },
    /// Scroll one genomic panel's read stack (the column of read tracks).
    ReadStackScrollPoints {
        /// Positive stable ID of the panel, as reported in the workspace snapshot
        /// (`panels[].id`).
        panel_id: PanelId,
        /// Vertical scroll offset of this panel's read stack in logical points from the top (0
        /// = top), non-negative. Clamped to the stack's maximum.
        #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
        value: f32,
    },
    /// Scroll one alignment track's read panel in one genomic panel.
    ReadPanelScrollPoints {
        /// Positive stable ID of the panel, as reported in the workspace snapshot
        /// (`panels[].id`).
        panel_id: PanelId,
        /// Positive stable ID of the alignment track whose read panel to scroll.
        track_id: TrackId,
        /// Vertical scroll offset in logical points from the top (0 = top), non-negative.
        /// Clamped to the maximum reported in the panel's `read_panel_states`.
        #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
        value: f32,
    },
    /// Override the read-row density of one alignment track in one genomic panel.
    ReadPanelDensity {
        /// Positive stable ID of the panel, as reported in the workspace snapshot
        /// (`panels[].id`).
        panel_id: PanelId,
        /// Positive stable ID of the alignment track whose density to override.
        track_id: TrackId,
        /// Read-row height multiplier, 0.25 to 4: a row is 7 logical points tall at 1.0 and
        /// larger values draw taller rows. The viewer raises values below 0.35 to 0.35 and
        /// reports the result as `clamped`.
        #[cfg_attr(feature = "schema", schemars(range(min = 0.25, max = 4.0)))]
        value: f32,
    },
}

impl AutomationSetting {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::ShowReads {
                track_id: Some(track_id),
                ..
            } => validate_track_id(*track_id),
            Self::WindowSize { width, height } => ((640..=3_840).contains(width)
                && (400..=2_160).contains(height))
            .then_some(())
            .ok_or_else(|| {
                "window size must be between 640x400 and 3840x2160 logical points".to_owned()
            }),
            Self::InterfaceScalePercent { value } => (rgv_core::MINIMUM_INTERFACE_SCALE_PERCENT
                ..=rgv_core::MAXIMUM_INTERFACE_SCALE_PERCENT)
                .contains(value)
                .then_some(())
                .ok_or_else(|| "interface scale must be between 75 and 200 percent".to_owned()),
            Self::ReadBrowsingBufferPercent { value } => {
                (rgv_core::MINIMUM_READ_BROWSING_BUFFER_PERCENT
                    ..=rgv_core::MAXIMUM_READ_BROWSING_BUFFER_PERCENT)
                    .contains(value)
                    .then_some(())
                    .ok_or_else(|| {
                        format!(
                            "read browsing buffer must be between {} and {} percent",
                            rgv_core::MINIMUM_READ_BROWSING_BUFFER_PERCENT,
                            rgv_core::MAXIMUM_READ_BROWSING_BUFFER_PERCENT
                        )
                    })
            }
            Self::TotalCacheBudgetMib { value } => (rgv_core::MINIMUM_CACHE_BUDGET_MIB
                ..=rgv_core::MAXIMUM_CACHE_BUDGET_MIB)
                .contains(value)
                .then_some(())
                .ok_or_else(|| {
                    format!(
                        "total cache budget must be between {} and {} MiB",
                        rgv_core::MINIMUM_CACHE_BUDGET_MIB,
                        rgv_core::MAXIMUM_CACHE_BUDGET_MIB
                    )
                }),
            Self::PropertiesPanelWidth { value } => (value.is_finite()
                && (280.0..=520.0).contains(value))
            .then_some(())
            .ok_or_else(|| {
                "Properties panel width must be finite and between 280 and 520".to_owned()
            }),
            Self::AlignmentColorMode { track_ids, value } => {
                validate_track_selection(track_ids)?;
                if let AlignmentColorMode::Tag(tag) = value {
                    validate_sam_tag_bytes(*tag)?;
                }
                Ok(())
            }
            Self::AlignmentViewFilter { track_ids, value }
            | Self::AlignmentHighlightFilter { track_ids, value } => {
                validate_track_selection(track_ids)?;
                rgv_core::validate_alignment_predicate_expression(value)
                    .map_err(|error| error.to_string())
            }
            Self::ReadStackingMode { track_ids, .. } => validate_track_selection(track_ids),
            Self::CompleteReadLimit { track_ids, value } => {
                validate_track_selection(track_ids)?;
                (1_000..=MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT)
                    .contains(value)
                    .then_some(())
                    .ok_or_else(|| {
                        format!(
                            "complete read limit must be between 1000 and {MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT}"
                        )
                    })
            }
            Self::RebuildStableReadArchitecture {
                panel_id,
                track_ids,
            } => {
                validate_panel_id(*panel_id)?;
                validate_track_selection(track_ids)
            }
            Self::AlignmentDensity { value, track_id }
                if value.is_finite() && (0.25..=4.0).contains(value) =>
            {
                track_id.map_or(Ok(()), validate_track_id)
            }
            Self::AlignmentDensity { .. } => {
                Err("alignment density must be finite and between 0.25 and 4".to_owned())
            }
            Self::TrackHeight { track_id, value } => {
                validate_track_id(*track_id)?;
                if value.is_finite() && (40.0..=1_200.0).contains(value) {
                    Ok(())
                } else {
                    Err("track height must be finite and between 40 and 1200".to_owned())
                }
            }
            Self::TrackCollapsed { track_id, .. } | Self::TrackVisible { track_id, .. } => {
                validate_track_id(*track_id)
            }
            Self::PresentationNodeVisible { target, .. } => match target {
                rgv_core::PresentationNodeId::Reference
                | rgv_core::PresentationNodeId::Track { .. } => Ok(()),
                rgv_core::PresentationNodeId::Group { .. } => Err(
                    "presentation group visibility must use presentation_group_visible".to_owned(),
                ),
            },
            Self::PresentationNodeHeight { target, value } => {
                if matches!(target, rgv_core::PresentationNodeId::Group { .. }) {
                    return Err(
                        "presentation groups do not have a resizable content row".to_owned()
                    );
                }
                let valid = value.is_finite()
                    && match target.channel() {
                        rgv_core::TrackChannel::Annotation => (26.0..=1_200.0).contains(value),
                        rgv_core::TrackChannel::Reads => (40.0..=1_200.0).contains(value),
                    };
                valid.then_some(()).ok_or_else(|| {
                    "presentation node height is outside the valid range for its channel".to_owned()
                })
            }
            Self::PresentationGroupExpanded { target, .. }
            | Self::PresentationGroupVisible { target, .. }
            | Self::PresentationUngroup { target } => {
                matches!(target, rgv_core::PresentationNodeId::Group { .. })
                    .then_some(())
                    .ok_or_else(|| "the setting target must be a presentation group".to_owned())
            }
            Self::PresentationCompositeVisible {
                channel,
                composite_id,
                ..
            } => (*channel == rgv_core::TrackChannel::Annotation && *composite_id > 0)
                .then_some(())
                .ok_or_else(|| {
                    "presentation composite must be a positive Annotation composite ID".to_owned()
                }),
            Self::PresentationGroup { track_ids, .. } => {
                validate_track_selection(track_ids)?;
                if track_ids.len() < 2 {
                    return Err("presentation groups require at least two tracks".to_owned());
                }
                let unique = track_ids.iter().copied().collect::<BTreeSet<_>>();
                (unique.len() == track_ids.len())
                    .then_some(())
                    .ok_or_else(|| "presentation group track_ids must be unique".to_owned())
            }
            Self::ReadPaneHeight { value } => (value.is_finite()
                && (100.0..=1_200.0).contains(value))
            .then_some(())
            .ok_or_else(|| "read pane height must be finite and between 100 and 1200".to_owned()),
            Self::VerticalScrollPoints {
                track_id: Some(track_id),
                ..
            } if track_id.get() == 0 => Err("track_id must be positive".to_owned()),
            Self::VerticalScrollPoints {
                panel_id, value, ..
            }
            | Self::AnnotationScrollPoints { panel_id, value }
            | Self::ReadStackScrollPoints { panel_id, value } => {
                validate_panel_id(*panel_id)?;
                if value.is_finite() && *value >= 0.0 {
                    Ok(())
                } else {
                    Err("vertical scroll must be finite and non-negative".to_owned())
                }
            }
            Self::ReadPanelScrollPoints {
                panel_id,
                track_id,
                value,
            } => {
                validate_panel_id(*panel_id)?;
                validate_track_id(*track_id)?;
                (value.is_finite() && *value >= 0.0)
                    .then_some(())
                    .ok_or_else(|| "read-panel scroll must be finite and non-negative".to_owned())
            }
            Self::ReadPanelDensity {
                panel_id,
                track_id,
                value,
            } => {
                validate_panel_id(*panel_id)?;
                validate_track_id(*track_id)?;
                (value.is_finite() && (0.25..=4.0).contains(value))
                    .then_some(())
                    .ok_or_else(|| "read-panel density must be between 0.25 and 4".to_owned())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "selector", rename_all = "snake_case")]
pub enum AutomationAlignmentSelector {
    /// Find records by exact read name.
    ExactReadName {
        /// Exact, case-sensitive read name (QNAME); non-empty, at most 16384 bytes. Treated as
        /// sensitive: it is matched inside the viewer, never echoed in results, and redacted
        /// when a batch is serialised.
        read_name: String,
    },
    /// Find records covering one reference base.
    Position {
        /// Zero-based genomic position on the panel's current contig; matches records whose
        /// aligned span `[start, end)` contains it.
        position: u64,
    },
    /// Find records overlapping a zero-based, half-open interval `[start, end)`.
    Interval {
        /// Zero-based inclusive start on the panel's current contig, in bases.
        start: u64,
        /// Zero-based exclusive end (half-open `[start, end)`); must be greater than `start`.
        /// Records overlapping the interval by at least one base match.
        end: u64,
    },
    /// Find records by alignment properties; every supplied property must match.
    Semantic {
        /// Required alignment state. Omit to accept any.
        #[serde(default)]
        alignment_state: Option<AutomationAlignmentState>,
        /// Required pair classification, compared case-insensitively with no separators:
        /// `unpaired`, `readunmapped`, `mateunmapped`, `matemissing`, `intercontig`,
        /// `discordantorientation`, `discordanttemplatelengthshort`,
        /// `discordanttemplatelengthlong`, `proper`, or `concordant` (the `pair_class` values
        /// returned in hits). Omit to accept any.
        #[serde(default)]
        pair_class: Option<String>,
        /// True requires a mapped mate with a known contig and position, false requires the
        /// absence of one. Omit to accept either.
        #[serde(default)]
        has_mapped_mate: Option<bool>,
        /// True requires a navigable supplementary (split-read) chain of more than one segment,
        /// false requires none. Omit to accept either.
        #[serde(default)]
        has_supplementary_chain: Option<bool>,
        /// True requires at least one base-modification call, false requires none. The search
        /// fails with `effective_state_unavailable` when modification evidence is not loaded
        /// at the current detail. Omit to accept either.
        #[serde(default)]
        has_modified_bases: Option<bool>,
    },
}

impl AutomationAlignmentSelector {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::ExactReadName { read_name } => validate_string(read_name, "read name"),
            Self::Interval { start, end } if start >= end => {
                Err("alignment interval start must be smaller than end".to_owned())
            }
            Self::Semantic { pair_class, .. } => {
                validate_optional_string(pair_class.as_deref(), "pair class")
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationAlignmentState {
    /// Neither secondary (SAM flag 0x100) nor supplementary (0x800).
    Primary,
    /// Secondary alignment (SAM flag 0x100 set).
    Secondary,
    /// Supplementary alignment (SAM flag 0x800 set).
    Supplementary,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationAlignmentReveal {
    /// Leave the viewport unchanged.
    #[default]
    None,
    /// Keep the current span and centre it on the record's midpoint.
    Center,
    /// Fit the viewport to the record's aligned span.
    Fit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationAlignmentRelationship {
    /// The selected read's mate.
    Mate,
    /// One segment of the selected read's supplementary (split-read) chain, chosen by
    /// `segment_index`.
    SupplementarySegment,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationNavigationDestination {
    /// Retarget the active panel.
    #[default]
    CurrentPanel,
    /// Open the destination in a new panel.
    NewPanel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AutomationReviewNavigation {
    /// Navigate to one review item.
    Item {
        /// Positive stable review-item ID as reported by `describe_review_queue`.
        #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
        review_item_id: u64,
    },
    /// Navigate to the first review item whose status is still pending.
    NextPending,
}

impl AutomationReviewNavigation {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Item { review_item_id: 0 } => Err("review_item_id must be positive".to_owned()),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AutomationHistoryNavigation {
    /// Step to the previous history entry. Fails with `history_boundary` at the oldest entry.
    Back,
    /// Step to the next history entry. Fails with `history_boundary` at the newest entry.
    Forward,
    /// Jump to one history entry.
    Index {
        /// Zero-based index of the history entry, as reported in `entries[].index` by
        /// `describe_navigation_history`.
        index: usize,
    },
}

fn default_alignment_result_limit() -> usize {
    25
}

fn default_feature_result_limit() -> usize {
    100
}

const fn default_read_browsing_buffer_percent() -> u16 {
    rgv_core::DEFAULT_READ_BROWSING_BUFFER_PERCENT
}

/// Constant-size alignment-scene diagnostics carried by `describe_workspace`. Collecting these
/// counters must never enumerate the retained scene or allocation ledger.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutomationAlignmentSceneDiagnostics {
    pub aggregate_cache_budget_bytes: usize,
    pub aggregate_cache_occupied_bytes: usize,
    pub contextual_gpu_reserved_bytes: usize,
    pub generic_gpu_reserved_bytes: usize,
    pub publication_waiting_for_capacity: bool,
    pub cross_owner_reclaim_active: bool,
    pub last_capacity_pressure_cause: String,
    pub last_capacity_pressure_requested_bytes: usize,
    pub last_capacity_pressure_effective_occupied_bytes: usize,
    pub last_capacity_pressure_retirement_credit_bytes: usize,
    pub last_capacity_pressure_limit_bytes: usize,
    pub last_capacity_pressure_required_reclaim_bytes: usize,
    pub optional_residency_phase: String,
    pub optional_residency_retention_limited: bool,
    pub camera_updates: u64,
    pub camera_only_updates: u64,
    pub source_acquisitions: u64,
    pub row_atlas_candidates: u64,
    pub presentation_candidates: u64,
    pub scene_publications: u64,
    pub optional_work_pauses: u64,
    pub optional_work_resumes: u64,
    pub row_activity_queries: u64,
    pub row_activity_rebuilds: u64,
    pub row_activity_intervals_examined: u64,
    pub row_activity_intervals_changed: u64,
    pub read_content_manifest_builds: u64,
    pub read_content_manifest_pages_enumerated: u64,
    pub read_prefetch_keys_inspected: u64,
    pub read_reclaim_keys_inspected: u64,
    pub summary_content_manifest_builds: u64,
    pub summary_content_manifest_pages_enumerated: u64,
    pub summary_prefetch_keys_inspected: u64,
    pub summary_reclaim_keys_inspected: u64,
    pub read_frame_mandatory_pages: usize,
    pub read_frame_body_instances: usize,
    pub read_frame_overlay_instances: usize,
    pub read_frame_labels: usize,
    pub read_frame_label_anchors_examined: usize,
    pub read_frame_page_pin_acquisitions: usize,
    pub read_frame_camera_bytes: usize,
    pub read_frame_immutable_upload_bytes: usize,
    pub read_frame_cache_hits: usize,
    pub read_frame_cache_misses: usize,
    pub summary_frame_mandatory_pages: usize,
    pub summary_frame_depth_instances: usize,
    pub summary_frame_allele_instances: usize,
    pub summary_frame_junction_instances: usize,
    pub summary_frame_modification_instances: usize,
    pub summary_frame_range_queries: usize,
    pub summary_frame_junction_entries_examined: usize,
    pub summary_frame_page_pin_acquisitions: usize,
    pub summary_frame_camera_bytes: usize,
    pub summary_frame_immutable_upload_bytes: usize,
    pub summary_frame_cache_hits: usize,
    pub summary_frame_cache_misses: usize,
    pub contextual_frame_camera_writes: usize,
    pub contextual_frame_immutable_upload_bytes: usize,
    pub contextual_text_anchors_examined: usize,
    pub contextual_text_anchors_available: usize,
    pub contextual_content_preparations: u64,
    pub contextual_camera_capture_attempts: u64,
    pub contextual_camera_captures: u64,
    pub contextual_content_replacement_requests: u64,
    pub contextual_invalid_camera_captures: u64,
    pub contextual_total_immutable_upload_bytes: u64,
    pub contextual_total_camera_writes: u64,
    pub generic_frame_uploaded_bytes: usize,
    pub generic_frame_slab_cache_hit: bool,
    pub generic_preparations: u64,
    pub generic_instances_examined: u64,
    pub generic_slab_cache_hits: u64,
    pub generic_total_uploaded_bytes: u64,
    pub generic_buffer_reallocations: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationWorkspaceSnapshot {
    pub application_version: String,
    pub schema_version: u32,
    #[serde(default)]
    pub application_instance_id: String,
    #[serde(default)]
    pub workspace_revision: u64,
    pub genome: AutomationGenomeSnapshot,
    pub settled: bool,
    pub active_panel_id: PanelId,
    pub panels: Vec<AutomationPanelSnapshot>,
    pub tracks: Vec<AutomationTrackSnapshot>,
    #[serde(default = "default_read_browsing_buffer_percent")]
    pub read_browsing_buffer_percent: u16,
    #[serde(default)]
    pub total_cache_budget_mib: usize,
    #[serde(default)]
    pub alignment_scene_budget_bytes: usize,
    #[serde(default)]
    pub alignment_scene_charged_bytes: usize,
    #[serde(default)]
    pub alignment_scene_reserved_bytes: usize,
    #[serde(default)]
    pub alignment_scene_transition_reserve_bytes: usize,
    #[serde(default)]
    pub alignment_scene_transition_reserved_bytes: usize,
    #[serde(default)]
    pub alignment_scene_transient_budget_bytes: usize,
    #[serde(default)]
    pub alignment_scene_transient_bytes: usize,
    #[serde(default)]
    pub alignment_scene_transient_high_water_bytes: usize,
    #[serde(default)]
    pub alignment_scene_pinned_bytes: usize,
    #[serde(default)]
    pub alignment_scene_pinned_overage_bytes: usize,
    #[serde(default)]
    pub alignment_scene_active_replacement_bundle: Option<u64>,
    #[serde(default)]
    pub alignment_scene_budget_refreshes: u64,
    #[serde(default)]
    pub alignment_scene_diagnostics: AutomationAlignmentSceneDiagnostics,
    #[serde(default)]
    pub read_pane_height: f32,
    #[serde(default = "automation_default_true")]
    pub annotation_section_expanded: bool,
    #[serde(default = "automation_default_true")]
    pub read_section_expanded: bool,
    #[serde(default)]
    pub presentation_groups: Vec<AutomationPresentationGroupSnapshot>,
    #[serde(default)]
    pub presentation_composites: Vec<AutomationPresentationCompositeSnapshot>,
    #[serde(default)]
    pub presentation_nodes: Vec<AutomationPresentationNodeSnapshot>,
    #[serde(default)]
    pub pane_layouts: Vec<AutomationPaneLayoutSnapshot>,
    #[serde(default)]
    pub workspace_layout: Option<AutomationWorkspaceLayoutSnapshot>,
    #[serde(default)]
    pub active_track_gesture: Option<AutomationTrackGestureSnapshot>,
    #[serde(default)]
    pub selection: Option<AutomationSelectionSnapshot>,
    #[serde(default)]
    pub status: Vec<String>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationPresentationGroupSnapshot {
    pub channel: rgv_core::TrackChannel,
    pub id: u32,
    pub label: String,
    pub members: Vec<TrackId>,
    pub expanded: bool,
    pub content_visible: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationPresentationCompositeSnapshot {
    pub channel: rgv_core::TrackChannel,
    pub id: u32,
    pub content_visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPresentationNodeSnapshot {
    pub id: rgv_core::PresentationNodeId,
    pub order_index: usize,
    pub desired_content_visible: bool,
    pub effective_content_visible: bool,
    #[serde(default)]
    pub preferred_height: Option<f32>,
    #[serde(default)]
    pub parent_group_id: Option<u32>,
    #[serde(default)]
    pub parent_composite_id: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationRectSnapshot {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationCytobandAvailability {
    Loading,
    Present,
    #[default]
    Absent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationWorkspacePanelLayoutSnapshot {
    pub panel_id: PanelId,
    pub cytobands: AutomationCytobandAvailability,
    pub column: AutomationRectSnapshot,
    pub header: AutomationRectSnapshot,
    pub header_genomic: AutomationRectSnapshot,
    pub header_scrollbar: AutomationRectSnapshot,
    pub canvas: AutomationRectSnapshot,
    pub scrollbar: AutomationRectSnapshot,
    pub metadata_row: AutomationRectSnapshot,
    #[serde(default)]
    pub cytoband_row: Option<AutomationRectSnapshot>,
    pub overview_row: AutomationRectSnapshot,
    pub span_row: AutomationRectSnapshot,
    pub ruler_row: AutomationRectSnapshot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationWorkspaceLayoutSnapshot {
    pub revision: u64,
    #[serde(default)]
    pub recomputations: u64,
    #[serde(default)]
    pub settled_resize_query_refreshes: u64,
    #[serde(default)]
    pub resolution_query_refresh_pending: bool,
    pub workspace: AutomationRectSnapshot,
    #[serde(default)]
    pub properties: AutomationRectSnapshot,
    #[serde(default)]
    pub properties_expanded: bool,
    #[serde(default)]
    pub properties_resize_active: bool,
    #[serde(default = "default_automation_scale")]
    pub display_scale: f32,
    #[serde(default = "default_automation_scale")]
    pub accessibility_scale: f32,
    pub genome_context: AutomationRectSnapshot,
    pub track_viewport: AutomationRectSnapshot,
    pub context_gutter: AutomationRectSnapshot,
    pub track_gutter: AutomationRectSnapshot,
    pub has_cytoband_row: bool,
    pub panels: Vec<AutomationWorkspacePanelLayoutSnapshot>,
}

const fn default_automation_scale() -> f32 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPaneItemLayoutSnapshot {
    pub id: rgv_core::PresentationNodeId,
    pub row: AutomationRectSnapshot,
    pub header: AutomationRectSnapshot,
    #[serde(default)]
    pub content: Option<AutomationRectSnapshot>,
    pub clip: AutomationRectSnapshot,
    pub desired_content_visible: bool,
    pub effective_content_visible: bool,
    #[serde(default)]
    pub composite_id: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPaneLayoutSnapshot {
    pub panel_id: PanelId,
    pub channel: rgv_core::TrackChannel,
    pub pane: AutomationRectSnapshot,
    pub content_bottom: f32,
    pub scroll_points: f32,
    pub stack_scroll_points: f32,
    pub items: Vec<AutomationPaneItemLayoutSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTrackGestureKind {
    ResizeNode,
    ResizeReadPane,
    AnnotationScrollbar,
    ReadStackScrollbar,
    ReadScrollbar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationTrackGestureSnapshot {
    pub kind: AutomationTrackGestureKind,
    pub panel_id: PanelId,
    #[serde(default)]
    pub target: Option<rgv_core::PresentationNodeId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationLandmarkIntegrityState {
    #[default]
    Unavailable,
    /// No landmark catalogue was configured or found, so there is nothing to verify.
    NotConfigured,
    Unchecked,
    Verified,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationLandmarkTrackState {
    #[default]
    Unconfigured,
    Configured,
    Matched,
    Loaded,
    Visible,
    Suppressed,
    Failed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationLandmarkSnapshot {
    #[serde(default)]
    pub catalogue_schema_version: Option<u32>,
    #[serde(default)]
    pub resource_configured: bool,
    #[serde(default)]
    pub resolution_status: Option<rgv_core::AssemblyResolutionStatus>,
    #[serde(default)]
    pub evidence: Option<rgv_core::AssemblyMatchEvidence>,
    #[serde(default)]
    pub subset: bool,
    #[serde(default)]
    pub matched_bundle_id: Option<String>,
    #[serde(default)]
    pub matched_bundle_version: Option<String>,
    #[serde(default)]
    pub integrity: AutomationLandmarkIntegrityState,
    #[serde(default)]
    pub automatic_track_state: AutomationLandmarkTrackState,
    #[serde(default)]
    pub automatic_track_loaded: bool,
    #[serde(default)]
    pub automatic_track_visible: bool,
    #[serde(default)]
    pub suppressed: bool,
    #[serde(default)]
    pub gene_asset_format: Option<rgv_core::LandmarkAssetFormat>,
    #[serde(default)]
    pub gene_asset_sha256: Option<String>,
    #[serde(default)]
    pub search_ready: bool,
    /// Reference sequences outside the matched build (spike-ins, decoys and the like).
    #[serde(default)]
    pub extra_contigs: usize,
    /// Where the catalogue directory came from, when one was found.
    #[serde(default)]
    pub resource_source: Option<rgv_core::LandmarkRootSource>,
    /// The catalogue, integrity or installation failure, when there is one.
    #[serde(default)]
    pub catalogue_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationGenomeSnapshot {
    pub name: String,
    #[serde(default)]
    pub context_label: String,
    #[serde(default)]
    pub assembly_identity: Option<rgv_core::GenomeAssemblyIdentity>,
    #[serde(default)]
    pub suggested_assembly_identity: Option<rgv_core::GenomeAssemblyIdentity>,
    pub reference_configured: bool,
    pub reference_ready: bool,
    #[serde(default)]
    pub reference_contig_count: usize,
    #[serde(default)]
    pub navigation_contig_count: usize,
    #[serde(default)]
    pub reference_is_subset: bool,
    #[serde(default)]
    pub cytobands_loaded: bool,
    #[serde(default)]
    pub default_annotation_count: usize,
    #[serde(default)]
    pub gene_model_track_count: usize,
    #[serde(default)]
    pub contig_alias_count: usize,
    #[serde(default)]
    pub landmarks: AutomationLandmarkSnapshot,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPanelSnapshot {
    pub id: PanelId,
    pub locus: String,
    pub contig: String,
    pub start: u64,
    pub end: u64,
    pub detail_level: DetailLevel,
    #[serde(default)]
    pub pixel_width: f64,
    #[serde(default)]
    pub bases_per_pixel: f64,
    pub vertical_scroll_points: f32,
    #[serde(default)]
    pub maximum_vertical_scroll_points: f32,
    /// Workspace-wide annotation camera, repeated per panel so existing
    /// automation consumers do not require a schema change.
    #[serde(default)]
    pub annotation_scroll_points: f32,
    #[serde(default)]
    pub maximum_annotation_scroll_points: f32,
    #[serde(default)]
    pub read_stack_scroll_points: f32,
    #[serde(default)]
    pub maximum_read_stack_scroll_points: f32,
    #[serde(default)]
    pub render_generation: u64,
    #[serde(default)]
    pub settled: bool,
    #[serde(default)]
    pub navigation_transaction_active: bool,
    #[serde(default)]
    pub navigation_target_locus: Option<String>,
    #[serde(default)]
    pub navigation_target_generation: Option<u64>,
    #[serde(default)]
    pub navigation_age_ms: Option<u64>,
    #[serde(default)]
    pub navigation_invalidation_reason: Option<String>,
    #[serde(default)]
    pub render_lanes: Vec<AutomationRenderLaneSnapshot>,
    #[serde(default)]
    pub context_lanes: Vec<AutomationContextLaneSnapshot>,
    #[serde(default)]
    pub read_panel_states: Vec<AutomationReadPanelStateSnapshot>,
    #[serde(default)]
    pub selected_presentation_nodes: Vec<rgv_core::PresentationNodeId>,
    #[serde(default)]
    pub presentation_selection_anchor: Option<rgv_core::PresentationNodeId>,
    /// What each alignment track's coverage lane is drawn from in this panel. Absent in
    /// snapshots written before the coverage pyramid existed.
    #[serde(default)]
    pub coverage_sources: Vec<AutomationCoverageSourceSnapshot>,
}

/// The authority of one alignment track's coverage lane in one panel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationCoverageSourceSnapshot {
    pub track_id: TrackId,
    /// `read-evidence` (exact depth from loaded reads), `pyramid-exact` (coverage pyramid at
    /// the resolution of the view), `pyramid-coarse` (a coarser or partial pyramid stand-in,
    /// still refining), `index-density` (index-only placeholder before any coverage arrived)
    /// or `pending` (regional zoom with nothing to draw yet).
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationContextLaneSnapshot {
    pub lane: String,
    #[serde(default)]
    pub track_id: Option<TrackId>,
    pub source_readiness: String,
    pub gpu_readiness: String,
    #[serde(default)]
    pub source_locus: Option<String>,
    #[serde(default)]
    pub content_revision: Option<u64>,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub failure: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationReadPanelStateSnapshot {
    pub track_id: TrackId,
    pub scroll_points: f32,
    #[serde(default)]
    pub maximum_scroll_points: f32,
    pub content_origin_points: f32,
    #[serde(default)]
    pub latent_scroll_fraction: Option<f32>,
    pub content_bottom_points: f32,
    #[serde(default)]
    pub density: Option<f32>,
    #[serde(default)]
    pub effective_density: f32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationRenderLaneSnapshot {
    pub lane: String,
    pub configured: bool,
    pub rendered: bool,
    #[serde(default)]
    pub omission_reason: Option<AutomationRenderOmissionReason>,
    #[serde(default)]
    pub queried_items: usize,
    #[serde(default)]
    pub rendered_items: usize,
    #[serde(default)]
    pub sampled_items: usize,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub filtered_unmapped_items: usize,
    #[serde(default)]
    pub filtered_secondary_items: usize,
    #[serde(default)]
    pub filtered_supplementary_items: usize,
    #[serde(default)]
    pub filtered_user_items: usize,
    #[serde(default)]
    pub filtered_duplicate_items: usize,
    #[serde(default)]
    pub filtered_qc_failed_items: usize,
    #[serde(default)]
    pub regional_insertions_rendered: usize,
    #[serde(default)]
    pub regional_insertions_collapsed: usize,
    #[serde(default)]
    pub regional_insertions_omitted: usize,
    #[serde(default)]
    pub raw_maximum_coverage: u32,
    #[serde(default)]
    pub coverage_exact_complete: bool,
    #[serde(default)]
    pub depth_segments_omitted: u64,
    #[serde(default)]
    pub packed_row_count: usize,
    #[serde(default)]
    pub visible_occupied_rows: usize,
    #[serde(default)]
    pub visible_row_span: usize,
    #[serde(default)]
    pub natural_stack_height: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationRenderOmissionReason {
    OverviewDetailThreshold,
    Disabled,
    NoReference,
    NoData,
    Loading,
    TooDense,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationTrackSnapshot {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    /// SHA-256 of the redacted persisted source identity. This permits prerequisite checks
    /// without exposing a local path, URL credential, or signed query string.
    #[serde(default)]
    pub source_fingerprint: Option<String>,
    pub visible: bool,
    pub collapsed: bool,
    pub height: f32,
    #[serde(default = "automation_default_true")]
    pub source_available: bool,
    pub lifecycle: AutomationTrackLifecycle,
    #[serde(default)]
    pub failure: Option<String>,
    #[serde(default)]
    pub alignment: Option<AutomationAlignmentTrackSnapshot>,
}

const fn automation_default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTrackLifecycle {
    Opening,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationReadPresentationState {
    #[default]
    Unavailable,
    Checking,
    Complete,
    TooDense,
    Failed,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationAlignmentTrackSnapshot {
    pub group_mode: AlignmentGroupMode,
    pub group_order: AlignmentOrderDirection,
    pub sort_mode: AlignmentSortMode,
    pub sort_order: AlignmentOrderDirection,
    #[serde(default)]
    pub sort_anchor: Option<u64>,
    pub filter_expression: String,
    #[serde(default)]
    pub view_filter: AlignmentPredicateExpression,
    #[serde(default)]
    pub highlight_filter: AlignmentPredicateExpression,
    #[serde(default)]
    pub view_filter_fingerprint: u64,
    #[serde(default)]
    pub view_filter_eligible_records: usize,
    #[serde(default)]
    pub view_filter_matching_records: usize,
    /// Independent match count for each predicate in canonical expression order. Together with
    /// the exact eligible/matching totals this exposes why the logical-AND view excluded reads.
    #[serde(default)]
    pub view_filter_predicate_matches: Vec<usize>,
    /// Canonical predicate list corresponding index-for-index with
    /// `view_filter_predicate_matches`, including legacy controls compiled into the typed view.
    #[serde(default)]
    pub view_filter_effective_predicates: Vec<AlignmentViewPredicate>,
    #[serde(default)]
    pub view_as_pairs: bool,
    #[serde(default)]
    pub read_stacking_mode: ReadStackingMode,
    #[serde(default)]
    pub read_presentation_state: AutomationReadPresentationState,
    #[serde(default)]
    pub requested_complete_read_limit: usize,
    #[serde(default)]
    pub effective_complete_read_limit: usize,
    #[serde(default)]
    pub eligible_read_records: usize,
    #[serde(default)]
    pub complete_read_records: usize,
    #[serde(default)]
    pub refusal_eligible_read_records: usize,
    #[serde(default)]
    pub read_refusal_scope: Option<String>,
    /// Unified-scene navigation phase for this track in the active panel.
    #[serde(default)]
    pub scene_phase: String,
    /// Immutable scene revision rendered by the committed panel epoch.
    #[serde(default)]
    pub scene_revision: Option<String>,
    #[serde(default)]
    pub scene_target_revision: u64,
    #[serde(default)]
    pub scene_prepared_start: Option<u64>,
    #[serde(default)]
    pub scene_prepared_end: Option<u64>,
    #[serde(default)]
    pub scene_record_count: usize,
    #[serde(default)]
    pub scene_row_count: usize,
    #[serde(default)]
    pub scene_read_page_count: usize,
    #[serde(default)]
    pub scene_summary_page_count: usize,
    #[serde(default)]
    pub scene_target_complete: bool,
    #[serde(default)]
    pub scene_reads_complete: bool,
    #[serde(default)]
    pub scene_reference_ready: bool,
    #[serde(default)]
    pub scene_vertical_residency: String,
    #[serde(default)]
    pub scene_camera_updates: u64,
    #[serde(default)]
    pub scene_camera_only_updates: u64,
    #[serde(default)]
    pub scene_source_acquisitions: u64,
    #[serde(default)]
    pub scene_source_retargets: u64,
    #[serde(default)]
    pub scene_source_refusals: u64,
    #[serde(default)]
    pub scene_publications: u64,
    #[serde(default)]
    pub scene_foreground_submissions: u64,
    #[serde(default)]
    pub scene_speculation_submissions: u64,
    #[serde(default)]
    pub overlapping_mate_policy: OverlappingMatePolicy,
    #[serde(default)]
    pub color_mode: AlignmentColorMode,
    #[serde(default = "default_alignment_density")]
    pub alignment_density: f32,
    #[serde(default)]
    pub read_base_display_mode: ReadBaseDisplayMode,
    #[serde(default)]
    pub shade_read_bases_by_quality: bool,
    #[serde(default)]
    pub show_modified_bases: bool,
    #[serde(default)]
    pub modified_base_color_mode: ModifiedBaseColorMode,
    #[serde(default)]
    pub show_reference: bool,
    #[serde(default)]
    pub show_coverage: bool,
    #[serde(default)]
    pub coverage_scale_transform: CoverageScaleTransform,
    #[serde(default)]
    pub show_reads: bool,
    #[serde(default)]
    pub include_secondary: bool,
    #[serde(default)]
    pub include_supplementary: bool,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationSelectionSnapshot {
    #[serde(default)]
    pub selection_id: String,
    #[serde(default)]
    pub evidence_revision: u64,
    #[serde(default)]
    pub workspace_revision_at_selection: u64,
    pub panel_id: PanelId,
    /// Contig containing the retained evidence. This remains independent of
    /// the active viewport so visibility stays correct after navigation.
    #[serde(default)]
    pub contig: String,
    #[serde(default)]
    pub track_id: Option<TrackId>,
    pub kind: AutomationSelectionKind,
    #[serde(default)]
    pub granularity: Option<AutomationSelectionGranularity>,
    #[serde(default)]
    pub anchor_position: Option<u64>,
    #[serde(default)]
    pub evidence_start: Option<u64>,
    #[serde(default)]
    pub evidence_end: Option<u64>,
    #[serde(default)]
    pub position: Option<u64>,
    #[serde(default)]
    pub start: Option<u64>,
    #[serde(default)]
    pub end: Option<u64>,
    #[serde(default)]
    pub fields: Vec<AutomationEvidenceField>,
    #[serde(default)]
    pub provenance: Vec<AutomationEvidenceField>,
    #[serde(default)]
    pub visible_in_active_view: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub stale_reason: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    /// Large human-readable fields were summarized in the structured
    /// snapshot; exact values remain available through explicit export.
    #[serde(default)]
    pub presentation_truncated: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationSelectionGranularity {
    /// Select the evidence for exactly the requested base. When one-base evidence is not
    /// available at the current zoom, the panel's viewport is first fitted to that single base
    /// and the command waits up to 30 seconds for the query, which visibly changes the view.
    #[default]
    ExactBase,
    /// Select the currently rendered bin that contains the position, without changing zoom.
    VisibleBin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSelectionKind {
    Panel,
    Track,
    Position,
    Read,
    ReadEvent,
    Pileup,
    Modification,
    Junction,
    Feature,
    Variant,
    Gwas,
    Quantitative,
    CopyNumber,
    Mutation,
    Interaction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationEvidenceField {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationReviewQueueSnapshot {
    pub application_instance_id: String,
    pub workspace_revision: u64,
    pub items: Vec<AutomationReviewItemSnapshot>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationReviewItemSnapshot {
    pub id: u64,
    pub label: String,
    pub locus: String,
    pub contig: String,
    pub start: u64,
    pub end: u64,
    pub status: ReviewStatus,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationNavigationHistorySnapshot {
    pub application_instance_id: String,
    pub workspace_revision: u64,
    pub panel_id: PanelId,
    pub entries: Vec<AutomationHistoryEntrySnapshot>,
    pub cursor: usize,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationHistoryEntrySnapshot {
    pub index: usize,
    pub locus: String,
    pub contig: String,
    pub start: u64,
    pub end: u64,
    pub current: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationAlignmentHit {
    pub handle: String,
    pub panel_id: PanelId,
    pub track_id: TrackId,
    pub contig: String,
    pub start: u64,
    pub end: u64,
    pub flags: u16,
    #[serde(default)]
    pub mapping_quality: Option<u8>,
    pub pair_class: String,
    pub mapped_mate_available: bool,
    pub supplementary_segment_count: usize,
    pub modification_code_count: usize,
    /// `present`, `absent`, `malformed`, or `unavailable`.
    #[serde(default)]
    pub modification_evidence_status: String,
}

/// One annotation feature returned by `query_features`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationFeature {
    /// Stable ID of the track the feature was read from.
    pub track_id: u64,
    /// The feature's display label (gene symbol, record name, or identifier); empty when the
    /// record carries none. At most 256 bytes.
    pub name: String,
    /// The source's feature type (`gene`, `exon`, ...), or `feature` when the format has none.
    pub feature_type: String,
    /// Contig name as the track's source spells it.
    pub contig: String,
    /// Zero-based inclusive start, in bases.
    pub start: u64,
    /// Zero-based exclusive end, in bases (half-open `[start, end)`).
    pub end: u64,
    /// `+`, `-`, or `.` when the feature is unstranded.
    pub strand: String,
    /// Gene symbol from the record's attributes, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gene_name: Option<String>,
}

/// The `data` of a successful `query_features` command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationFeatureQueryResult {
    /// Contig the tracks were queried on, as the workspace names it.
    pub contig: String,
    /// Zero-based inclusive start of the interval that was read.
    pub start: u64,
    /// Zero-based exclusive end of the interval that was read (clamped to the contig).
    pub end: u64,
    /// Stable IDs of the tracks that answered, in workspace order.
    #[serde(default)]
    pub track_ids: Vec<u64>,
    /// This page of features, ordered by track, `start`, `end`, then `name`.
    pub features: Vec<AutomationFeature>,
    /// Number of features overlapping the interval across the queried tracks, before paging.
    pub total_matching: usize,
    /// The paging offset this page starts at.
    #[serde(default)]
    pub offset: usize,
    /// True when matching features remain beyond this page.
    #[serde(default)]
    pub truncated: bool,
}

/// The feature `goto_gene` navigated to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationGeneHit {
    /// Stable ID of the track the feature was found in.
    pub track_id: u64,
    /// The feature's display label. At most 256 bytes.
    pub name: String,
    /// The source's feature type (`gene`, `transcript`, ...), or `feature` when it has none.
    pub feature_type: String,
}

/// The `data` of a successful `goto_gene` command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationGeneNavigationResult {
    /// Stable ID of the panel that was navigated.
    pub panel_id: u64,
    /// Contig the panel now shows.
    pub contig: String,
    /// Zero-based inclusive start of the interval the panel now shows.
    pub start: u64,
    /// Zero-based exclusive end of the interval the panel now shows.
    pub end: u64,
    /// The matched feature.
    pub hit: AutomationGeneHit,
    /// Number of features the search returned for the query (1 when the match was unique).
    pub candidate_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationAlignmentSearchResult {
    pub application_instance_id: String,
    pub workspace_revision: u64,
    pub query_generation: u64,
    pub query_contig: String,
    pub query_start: u64,
    pub query_end: u64,
    pub hits: Vec<AutomationAlignmentHit>,
    #[serde(default)]
    pub truncated: bool,
}

/// Checks a URL that automation asked the viewer to fetch.
///
/// A control client is confined to a filesystem root; without this it could
/// still use the viewer as a proxy to read `file:` URLs or services that only
/// the user's machine or network can reach. Only `http` and `https` are
/// accepted, and, unless `allow_private_network` is set (the
/// `--control-allow-private-urls` launch flag), the host may not be
/// `localhost` or a loopback, private, link-local, shared-address-space or
/// unspecified address literal. Host names are not resolved here.
pub fn check_automation_remote_url(
    url: &str,
    allow_private_network: bool,
) -> Result<(), AutomationError> {
    let refused = |reason: &str| AutomationError::RemoteSourceRefused {
        url: rgv_core::TrackSource::Remote {
            url: url.to_owned(),
        }
        .redacted_label(),
        reason: reason.to_owned(),
    };
    let parsed = url::Url::parse(url).map_err(|_| refused("it is not a valid URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(refused("only http and https URLs are accepted"));
    }
    if allow_private_network {
        return Ok(());
    }
    let private = match parsed.host() {
        None => return Err(refused("it has no host")),
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(address)) => ipv4_is_private_network(address),
        Some(url::Host::Ipv6(address)) => {
            let segments = address.segments();
            address.is_loopback()
                || address.is_unspecified()
                // Unique local fc00::/7 and link-local fe80::/10.
                || segments[0] & 0xfe00 == 0xfc00
                || segments[0] & 0xffc0 == 0xfe80
                || address
                    .to_ipv4_mapped()
                    .is_some_and(ipv4_is_private_network)
        }
    };
    if private {
        return Err(refused(
            "loopback, link-local and private-network addresses are not reachable from \
             automation unless RGV was started with --control-allow-private-urls",
        ));
    }
    Ok(())
}

fn ipv4_is_private_network(address: std::net::Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        // Shared address space 100.64.0.0/10.
        || (octets[0] == 100 && octets[1] & 0xc0 == 64)
}

/// Applies the control-root rules to every source a project names, for a
/// project opened by a control client: local sources must really lie inside
/// `root` (symbolic links resolved, as in
/// [`AutomationBatch::resolve_paths_within`]) and remote ones must pass
/// [`check_automation_remote_url`]. Relative local sources must already have
/// been resolved against the project's directory, as `SessionDocument::read`
/// does.
pub fn check_session_sources_within(
    document: &rgv_core::SessionDocument,
    root: impl AsRef<Path>,
    allow_private_network: bool,
) -> Result<(), AutomationError> {
    let root = normalize_automation_path(&std::path::absolute(root)?);
    let real_root = real_automation_path(&root)?;
    let roots = ControlRoot {
        lexical: &root,
        real: &real_root,
    };
    let sources = document
        .reference
        .iter()
        .chain(&document.cytobands)
        .chain(document.alignment_tracks.iter().map(|track| &track.source))
        .chain(document.tracks.iter().map(|track| &track.source));
    for source in sources {
        match source {
            TrackSource::Local { path } => {
                roots.contain(path)?;
            }
            TrackSource::Remote { url } => check_automation_remote_url(url, allow_private_network)?,
        }
    }
    let index_urls = document.remote_reference_index_url.iter().chain(
        document
            .alignment_tracks
            .iter()
            .filter_map(|track| track.remote_index_url.as_ref()),
    );
    for url in index_urls {
        check_automation_remote_url(url, allow_private_network)?;
    }
    Ok(())
}

/// A control root as the caller named it and as the filesystem resolves it.
struct ControlRoot<'a> {
    lexical: &'a Path,
    real: &'a Path,
}

impl ControlRoot<'_> {
    /// The lexical absolute form of `value` under the root, provided its real
    /// location is inside the real root.
    fn contain(&self, value: &str) -> Result<PathBuf, AutomationError> {
        let path = Path::new(value);
        let outside = || AutomationError::PathOutsideControlRoot(path.display().to_string());
        let candidate = if path.is_relative() {
            self.lexical.join(path)
        } else {
            path.to_owned()
        };
        let resolved = normalize_automation_path(&candidate);
        // A lexical prefix test is defeated by a symbolic link inside the
        // root (and wrongly refuses the root's own real spelling), so the
        // real location is what must lie inside the real root.
        let real = real_automation_path(&resolved).map_err(|_| outside())?;
        if !real.starts_with(self.real) {
            return Err(outside());
        }
        Ok(resolved)
    }
}

/// Resolves symbolic links in the deepest existing ancestor of an absolute,
/// lexically normalized path and re-appends the components that do not exist
/// yet. A dangling symbolic link is an error rather than a "missing" name,
/// because creating the file would follow the link to wherever it points.
fn real_automation_path(path: &Path) -> std::io::Result<PathBuf> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut real) => {
                for component in missing.iter().rev() {
                    real.push(component);
                }
                return Ok(real);
            }
            Err(error) => {
                if ancestor
                    .symlink_metadata()
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    return Err(error);
                }
                let (Some(name), Some(parent)) = (ancestor.file_name(), ancestor.parent()) else {
                    return Err(error);
                };
                missing.push(name.to_os_string());
                ancestor = parent;
            }
        }
    }
}

fn normalize_automation_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                let _ = normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum AutomationOptionScope {
    /// A workspace-wide option.
    Workspace,
    /// An option of one track.
    Track {
        /// Positive stable ID of the track the option applies to.
        track_id: TrackId,
    },
    /// An option of one panel.
    Panel {
        /// Positive stable ID of the panel the option applies to.
        panel_id: PanelId,
    },
}

impl AutomationOptionScope {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Workspace => Ok(()),
            Self::Track { track_id } => validate_track_id(*track_id),
            Self::Panel { panel_id } if panel_id.get() > 0 => Ok(()),
            Self::Panel { .. } => Err("panel_id must be positive".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum AutomationSelection {
    /// Select a track in the active panel.
    Track {
        /// Positive stable ID of the track to select.
        track_id: TrackId,
    },
    /// Make a panel the active panel.
    Panel {
        /// Positive stable ID of the panel to activate.
        panel_id: PanelId,
    },
    /// Select the evidence at one genomic position of a track in a panel: the pileup column of
    /// an alignment track, or on an annotation track the shortest feature or variant record
    /// covering the position, or the quantitative record at it. The panel becomes active and,
    /// when the position is outside its viewport, is recentred on it.
    Position {
        /// Positive stable ID of the panel to select in.
        panel_id: PanelId,
        /// Positive stable ID of the track whose evidence to select.
        track_id: TrackId,
        /// Zero-based genomic position on the panel's current contig (one less than the
        /// one-based coordinate shown in a locus string). Must be less than the contig length.
        position: u64,
        /// How precisely the evidence is resolved (default `exact_base`).
        #[serde(default)]
        granularity: AutomationSelectionGranularity,
    },
}

impl AutomationSelection {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Track { track_id } => validate_track_id(*track_id),
            Self::Panel { panel_id } if panel_id.get() > 0 => Ok(()),
            Self::Panel { .. } => Err("panel_id must be positive".to_owned()),
            Self::Position {
                panel_id, track_id, ..
            } => {
                if panel_id.get() == 0 {
                    return Err("panel_id must be positive".to_owned());
                }
                validate_track_id(*track_id)
            }
        }
    }
}

fn default_alignment_density() -> f32 {
    1.0
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationSnapshotScope {
    /// The committed workspace rectangle: every panel's tracks, without the application shell.
    #[default]
    VisibleWorkspace,
    /// The active panel's visible canvas only.
    ActivePanel,
    /// The active panel's complete content. Refused, without writing a partial file, unless
    /// that content is already unscrolled and fits the captured canvas.
    FullActivePanel,
    /// The whole native window client area, including menus, the Properties panel, and status.
    /// PNG only, at the window's own pixel density.
    NativeWindow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationSnapshotFormat {
    /// Raster PNG image.
    #[default]
    Png,
    /// Vector SVG image. Not available for the `native_window` scope.
    Svg,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AutomationExportKind {
    /// Backward-compatible alias for the committed filtered alignment view.
    Alignments,
    /// One row per alignment record that passes the view filter, including its read name.
    /// Fails with `too_dense` when individual reads are not available at the current zoom.
    FilteredAlignments,
    /// One row per alignment record eligible for the view before the view filter is applied,
    /// including its read name. Fails with `too_dense` when individual reads are not
    /// available at the current zoom.
    EligibleAlignments,
    /// One row per covered base: depth, strand depths, A/C/G/T/N, insertion and deletion
    /// counts, and mean base and mapping quality. Bases with no coverage are omitted.
    Pileup,
    /// The retained evidence of the current selection. Fails with `no_selection` when nothing
    /// is selected and `selection_stale` when the evidence is stale.
    SelectedEvidence,
    /// Base-modification summaries per position. Requires modified-base frequency collection
    /// to be enabled.
    Modifications,
    /// Splice-junction summaries of the visible alignment tracks.
    Junctions,
    /// Variant records of the visible variant tracks, with their INFO fields and genotypes.
    Variants,
    /// Value windows of the visible quantitative tracks.
    Quantitative,
    /// The workspace's regions of interest in BED format (zero-based, half-open).
    Regions,
    /// The names of the workspace's tracks.
    TrackNames,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationErrorCode {
    InvalidBatch,
    UnsupportedVersion,
    InvalidCommand,
    UnsupportedCommand,
    ResourceNotFound,
    InvalidState,
    InvalidSettingScope,
    SettingOutOfRange,
    EffectiveStateUnavailable,
    TooDense,
    NoSelection,
    SelectionStale,
    StaleHandle,
    AmbiguousAlignmentMatch,
    RelatedAlignmentUnavailable,
    ReviewItemNotFound,
    HistoryBoundary,
    UnsavedChanges,
    WorkspaceReplacementNotConfirmed,
    WorkspaceRevisionConflict,
    Timeout,
    Io,
    Internal,
    /// Several annotation features matched a `goto_gene` query and none was preferred.
    AmbiguousMatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationCommandStatus {
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationCommandResult {
    pub command_index: usize,
    #[serde(default)]
    pub command: String,
    pub status: AutomationCommandStatus,
    #[serde(default)]
    pub error_code: Option<AutomationErrorCode>,
    pub message: String,
    #[serde(default)]
    pub data: serde_json::Value,
    #[serde(default)]
    pub elapsed_ms: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationEnvironmentTelemetry {
    #[serde(default)]
    pub application_version: String,
    #[serde(default)]
    pub operating_system: String,
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub gpu_adapter: String,
    #[serde(default)]
    pub gpu_backend: String,
    #[serde(default)]
    pub surface_width_pixels: u32,
    #[serde(default)]
    pub surface_height_pixels: u32,
    #[serde(default)]
    pub scale_factor: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationFrameTelemetry {
    #[serde(default)]
    pub presented_frames: u64,
    #[serde(default)]
    pub samples_ms: Vec<f64>,
    #[serde(default)]
    pub samples_dropped: u64,
    #[serde(default)]
    pub median_ms: Option<f64>,
    #[serde(default)]
    pub p95_ms: Option<f64>,
    #[serde(default)]
    pub maximum_ms: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationGpuTelemetry {
    #[serde(default)]
    pub prepared_frames: u64,
    #[serde(default)]
    pub uploaded_frames: u64,
    #[serde(default)]
    pub total_uploaded_bytes: u64,
    #[serde(default)]
    pub peak_uploaded_bytes: usize,
    #[serde(default)]
    pub peak_instances: usize,
    #[serde(default)]
    pub buffer_reallocations: u64,
    #[serde(default)]
    pub truncated_frames: u64,
    #[serde(default)]
    pub slab_cache_hits: u64,
    #[serde(default)]
    pub peak_slab_entries: usize,
    #[serde(default)]
    pub peak_slab_resident_bytes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationNavigationTelemetry {
    #[serde(default)]
    pub panel_id: u64,
    #[serde(default)]
    pub start: u64,
    #[serde(default)]
    pub end: u64,
    #[serde(default)]
    pub input_to_first_presented_frame_ms: Option<f64>,
    #[serde(default)]
    pub input_to_authoritative_frame_ms: Option<f64>,
    #[serde(default)]
    pub presented_frames: u64,
    #[serde(default)]
    pub maximum_frame_ms: Option<f64>,
    #[serde(default)]
    pub scene_phase: String,
    #[serde(default)]
    pub foreground_submissions: u64,
    #[serde(default)]
    pub speculation_submissions: u64,
    #[serde(default)]
    pub source_acquisitions: u64,
    #[serde(default)]
    pub camera_only_updates: u64,
    #[serde(default)]
    pub scene_publications: u64,
    #[serde(default)]
    pub gpu_uploaded_bytes: u64,
    #[serde(default)]
    pub gpu_buffer_reallocations: u64,
    #[serde(default)]
    pub gpu_slab_cache_hits: u64,
    #[serde(default)]
    pub complete: bool,
}

/// A bounded, privacy-preserving timeline for the most recent vertical
/// scrollbar gesture. Events contain rendering metrics, never genomic data or
/// pointer positions in screen coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationInteractionTrace {
    pub gesture_id: u64,
    pub active: bool,
    pub complete: bool,
    pub events: Vec<AutomationInteractionTraceEvent>,
    #[serde(default)]
    pub events_dropped: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationInteractionTraceEvent {
    pub index: usize,
    pub elapsed_ms: f64,
    pub stage: String,
    #[serde(default)]
    pub duration_ms: Option<f64>,
    #[serde(default)]
    pub primitive_count: Option<usize>,
    #[serde(default)]
    pub secondary_count: Option<usize>,
    #[serde(default)]
    pub uploaded_bytes: Option<usize>,
    #[serde(default)]
    pub buffer_reallocated: Option<bool>,
    #[serde(default)]
    pub cpu_capacity_grew: Option<bool>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AutomationRunTelemetry {
    #[serde(default)]
    pub run_elapsed_ms: f64,
    #[serde(default)]
    pub startup_to_first_frame_ms: Option<f64>,
    #[serde(default)]
    pub startup_to_first_settled_ms: Option<f64>,
    #[serde(default)]
    pub environment: AutomationEnvironmentTelemetry,
    #[serde(default)]
    pub frames: AutomationFrameTelemetry,
    #[serde(default)]
    pub gpu: AutomationGpuTelemetry,
    #[serde(default)]
    pub navigation: Vec<AutomationNavigationTelemetry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationRunReport {
    pub schema_version: u32,
    pub succeeded: bool,
    pub results: Vec<AutomationCommandResult>,
    #[serde(default)]
    pub telemetry: AutomationRunTelemetry,
}

impl AutomationRunReport {
    pub fn to_json(&self) -> Result<String, AutomationError> {
        Ok(format!("{}\n", serde_json::to_string_pretty(self)?))
    }
}

/// Smallest and largest PNG pixel density an automation snapshot may request.
pub const MINIMUM_AUTOMATION_SNAPSHOT_SCALE: f32 = 0.25;
pub const MAXIMUM_AUTOMATION_SNAPSHOT_SCALE: f32 = 8.0;

/// Pixel density of an automation snapshot when the command does not name one.
#[must_use]
pub const fn default_snapshot_scale() -> f32 {
    1.0
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_default_snapshot_scale(scale: &f32) -> bool {
    scale.to_bits() == default_snapshot_scale().to_bits()
}

fn validate_snapshot_scale(scale: f32, scope: AutomationSnapshotScope) -> Result<(), String> {
    if !scale.is_finite()
        || !(MINIMUM_AUTOMATION_SNAPSHOT_SCALE..=MAXIMUM_AUTOMATION_SNAPSHOT_SCALE).contains(&scale)
    {
        return Err(format!(
            "snapshot scale must be between {MINIMUM_AUTOMATION_SNAPSHOT_SCALE} and {MAXIMUM_AUTOMATION_SNAPSHOT_SCALE}"
        ));
    }
    if scope == AutomationSnapshotScope::NativeWindow && !is_default_snapshot_scale(&scale) {
        return Err(
            "native_window snapshots are captured at the window's pixel density; scale must be 1"
                .to_owned(),
        );
    }
    Ok(())
}

fn default_settle_timeout_millis() -> u64 {
    30_000
}

fn validate_settle_timeout(timeout_millis: u64) -> Result<(), String> {
    if (MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS..=MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS)
        .contains(&timeout_millis)
    {
        Ok(())
    } else {
        Err(format!(
            "timeout_millis must be between {MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS} and {MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS}"
        ))
    }
}

fn validate_panel_id(panel_id: PanelId) -> Result<(), String> {
    if panel_id.get() == 0 {
        Err("panel_id must be positive".to_owned())
    } else {
        Ok(())
    }
}

fn validate_source(source: &TrackSource) -> Result<(), String> {
    match source {
        TrackSource::Local { path } => validate_string(path, "local source path"),
        TrackSource::Remote { url } => validate_string(url, "remote source URL"),
    }
}

fn validate_track_selection(track_ids: &[TrackId]) -> Result<(), String> {
    if track_ids.is_empty() || track_ids.len() > MAXIMUM_AUTOMATION_TRACK_SELECTION {
        return Err(format!(
            "track_ids must contain 1..={MAXIMUM_AUTOMATION_TRACK_SELECTION} entries"
        ));
    }
    for track_id in track_ids {
        validate_track_id(*track_id)?;
    }
    Ok(())
}

fn validate_track_id(track_id: TrackId) -> Result<(), String> {
    if track_id.get() == 0 {
        Err("track_id must be positive".to_owned())
    } else {
        Ok(())
    }
}

fn validate_optional_sam_tag(tag: Option<&str>) -> Result<(), String> {
    let Some(tag) = tag else {
        return Ok(());
    };
    let bytes = tag.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1].is_ascii_alphanumeric() {
        Ok(())
    } else {
        Err("tag must be a valid two-character SAM tag".to_owned())
    }
}

fn validate_sam_tag_bytes(tag: [u8; 2]) -> Result<(), String> {
    if tag[0].is_ascii_alphabetic() && tag[1].is_ascii_alphanumeric() {
        Ok(())
    } else {
        Err("tag must be a valid two-character SAM tag".to_owned())
    }
}

fn validate_optional_string(value: Option<&str>, label: &str) -> Result<(), String> {
    value.map_or(Ok(()), |value| validate_string(value, label))
}

fn validate_string(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAXIMUM_AUTOMATION_STRING_BYTES {
        Err(format!(
            "{label} must be non-empty and no more than {MAXIMUM_AUTOMATION_STRING_BYTES} bytes"
        ))
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum AutomationError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("automation schema version {0} is not supported")]
    UnsupportedVersion(u32),
    #[error("automation batch contains {0} commands; expected 1..={MAXIMUM_AUTOMATION_COMMANDS}")]
    InvalidCommandCount(usize),
    #[error("automation command {index} is invalid: {reason}")]
    InvalidCommand { index: usize, reason: String },
    #[error("automation path {0} is outside the selected local-control root")]
    PathOutsideControlRoot(String),
    #[error("automation may not open {url}: {reason}")]
    RemoteSourceRefused { url: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_batch_round_trip_covers_the_public_command_vocabulary() {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::DescribeWorkspace,
                AutomationCommand::DescribeSelection,
                AutomationCommand::NewWorkspace,
                AutomationCommand::LoadTrack {
                    source: TrackSource::Local {
                        path: "data/reads.bam".to_owned(),
                    },
                    kind: Some(TrackKind::Alignment),
                    name: Some("Tumour".to_owned()),
                },
                AutomationCommand::Goto {
                    loci: vec!["chr1:101-200".to_owned(), "chr2:301-400".to_owned()],
                },
                AutomationCommand::ChangeViewport {
                    panel_id: PanelId::new(1),
                    operation: AutomationViewportOperation::Zoom {
                        factor: 2.0,
                        anchor_position: Some(150),
                    },
                },
                AutomationCommand::WaitUntilSettled {
                    timeout_millis: 30_000,
                },
                AutomationCommand::Snapshot {
                    path: "output/view.png".to_owned(),
                    scope: AutomationSnapshotScope::VisibleWorkspace,
                    format: AutomationSnapshotFormat::Png,
                    scale: default_snapshot_scale(),
                },
                AutomationCommand::Save {
                    path: "output/project.json".to_owned(),
                },
                AutomationCommand::Exit,
            ],
        };

        let json = batch.to_json().unwrap();
        assert_eq!(AutomationBatch::from_json(&json).unwrap(), batch);
        assert_eq!(
            batch
                .commands
                .iter()
                .map(AutomationCommand::name)
                .collect::<Vec<_>>(),
            [
                "describe_workspace",
                "describe_selection",
                "new_workspace",
                "load_track",
                "goto",
                "change_viewport",
                "wait_until_settled",
                "snapshot",
                "save",
                "exit",
            ]
        );
    }

    #[test]
    fn native_window_snapshot_scope_round_trips() {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Snapshot {
                path: "captures/window.png".to_owned(),
                scope: AutomationSnapshotScope::NativeWindow,
                format: AutomationSnapshotFormat::Png,
                scale: default_snapshot_scale(),
            }],
        };
        let json = batch.to_json().unwrap();
        assert!(json.contains("native_window"));
        assert_eq!(AutomationBatch::from_json(&json).unwrap(), batch);
    }

    #[test]
    fn viewport_operations_reject_ambiguous_or_unbounded_values() {
        for operation in [
            AutomationViewportOperation::PanBases { delta: 0 },
            AutomationViewportOperation::PanFraction { fraction: f64::NAN },
            AutomationViewportOperation::PanFraction { fraction: 101.0 },
            AutomationViewportOperation::Zoom {
                factor: 0.0,
                anchor_position: None,
            },
            AutomationViewportOperation::Zoom {
                factor: f64::INFINITY,
                anchor_position: None,
            },
            AutomationViewportOperation::FitInterval { start: 10, end: 10 },
        ] {
            let batch = AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands: vec![AutomationCommand::ChangeViewport {
                    panel_id: PanelId::new(1),
                    operation,
                }],
            };
            assert!(matches!(
                batch.to_json(),
                Err(AutomationError::InvalidCommand { index: 0, .. })
            ));
        }
    }

    #[test]
    fn alignment_arrangement_anchors_round_trip_and_reject_irrelevant_modes() {
        let track = TrackId::new(7);
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::Group {
                    track_ids: vec![track],
                    mode: AlignmentGroupMode::Insertion,
                    direction: AlignmentOrderDirection::Natural,
                    tag: None,
                    anchor_position: Some(1_234),
                },
                AutomationCommand::Sort {
                    track_ids: vec![track],
                    mode: AlignmentSortMode::Base,
                    direction: AlignmentOrderDirection::Reverse,
                    tag: None,
                    anchor_position: Some(1_235),
                },
            ],
        };
        let json = batch.to_json().unwrap();
        assert_eq!(AutomationBatch::from_json(&json).unwrap(), batch);

        for command in [
            AutomationCommand::Group {
                track_ids: vec![track],
                mode: AlignmentGroupMode::Strand,
                direction: AlignmentOrderDirection::Natural,
                tag: None,
                anchor_position: Some(1_234),
            },
            AutomationCommand::Sort {
                track_ids: vec![track],
                mode: AlignmentSortMode::MappingQuality,
                direction: AlignmentOrderDirection::Natural,
                tag: None,
                anchor_position: Some(1_234),
            },
        ] {
            let invalid = AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands: vec![command],
            };
            assert!(matches!(
                invalid.to_json(),
                Err(AutomationError::InvalidCommand { index: 0, .. })
            ));
        }
    }

    #[test]
    fn version_one_batches_remain_readable_and_receive_exact_base_defaults() {
        let batch = AutomationBatch::from_json(
            r#"{
                "schema_version": 1,
                "commands": [{
                    "command": "select",
                    "selection": {
                        "selection": "position",
                        "panel_id": 1,
                        "track_id": 2,
                        "position": 149
                    }
                }]
            }"#,
        )
        .unwrap();
        assert!(matches!(
            batch.commands.as_slice(),
            [AutomationCommand::Select {
                selection: AutomationSelection::Position {
                    granularity: AutomationSelectionGranularity::ExactBase,
                    ..
                }
            }]
        ));
    }

    #[test]
    fn older_alignment_selection_commands_receive_safe_reveal_and_revision_defaults() {
        let batch = AutomationBatch::from_json(
            r#"{
                "schema_version": 2,
                "commands": [{
                    "command": "select_alignment",
                    "handle": "opaque-fixture-handle"
                }]
            }"#,
        )
        .unwrap();
        assert!(matches!(
            batch.commands.as_slice(),
            [AutomationCommand::SelectAlignment {
                reveal: AutomationAlignmentReveal::None,
                expected_workspace_revision: None,
                ..
            }]
        ));
    }

    #[test]
    fn sensitive_alignment_selectors_are_redacted_from_persisted_batches() {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::FindAlignments {
                panel_id: PanelId::new(1),
                track_id: TrackId::new(2),
                selector: AutomationAlignmentSelector::ExactReadName {
                    read_name: "private-read-name".to_owned(),
                },
                maximum_results: 1,
            }],
        };
        let json = batch.to_json().unwrap();
        assert!(!json.contains("private-read-name"));
        assert!(json.contains("[redacted]"));
    }

    #[test]
    fn typed_workspace_and_selection_snapshots_round_trip() {
        let selection = AutomationSelectionSnapshot {
            selection_id: "selection-fixture".to_owned(),
            evidence_revision: 3,
            workspace_revision_at_selection: 7,
            panel_id: PanelId::new(1),
            contig: "chr1".to_owned(),
            track_id: Some(TrackId::new(7)),
            kind: AutomationSelectionKind::Pileup,
            granularity: Some(AutomationSelectionGranularity::ExactBase),
            anchor_position: Some(149),
            evidence_start: Some(149),
            evidence_end: Some(150),
            position: Some(149),
            start: Some(149),
            end: Some(150),
            fields: vec![AutomationEvidenceField {
                key: "A".to_owned(),
                value: "12".to_owned(),
            }],
            provenance: Vec::new(),
            visible_in_active_view: true,
            stale: false,
            stale_reason: None,
            truncated: false,
            presentation_truncated: false,
        };
        let workspace = AutomationWorkspaceSnapshot {
            application_version: "0.1.0".to_owned(),
            schema_version: AUTOMATION_SCHEMA_VERSION,
            application_instance_id: "application-fixture".to_owned(),
            workspace_revision: 7,
            genome: AutomationGenomeSnapshot {
                name: "fixture".to_owned(),
                reference_configured: true,
                reference_ready: true,
                context_label: "fixture".to_owned(),
                assembly_identity: None,
                suggested_assembly_identity: None,
                reference_contig_count: 1,
                navigation_contig_count: 1,
                reference_is_subset: false,
                cytobands_loaded: false,
                default_annotation_count: 0,
                gene_model_track_count: 0,
                contig_alias_count: 0,
                landmarks: AutomationLandmarkSnapshot::default(),
            },
            settled: true,
            active_panel_id: PanelId::new(1),
            panels: vec![AutomationPanelSnapshot {
                id: PanelId::new(1),
                locus: "chr1:101-200".to_owned(),
                contig: "chr1".to_owned(),
                start: 100,
                end: 200,
                detail_level: DetailLevel::Reads,
                pixel_width: 1_000.0,
                bases_per_pixel: 0.1,
                vertical_scroll_points: 0.0,
                maximum_vertical_scroll_points: 0.0,
                annotation_scroll_points: 0.0,
                maximum_annotation_scroll_points: 0.0,
                read_stack_scroll_points: 0.0,
                maximum_read_stack_scroll_points: 0.0,
                render_generation: 1,
                settled: true,
                navigation_transaction_active: false,
                navigation_target_locus: None,
                navigation_target_generation: None,
                navigation_age_ms: None,
                navigation_invalidation_reason: None,
                render_lanes: Vec::new(),
                context_lanes: Vec::new(),
                read_panel_states: Vec::new(),
                selected_presentation_nodes: Vec::new(),
                presentation_selection_anchor: None,
                coverage_sources: Vec::new(),
            }],
            tracks: Vec::new(),
            read_browsing_buffer_percent: rgv_core::DEFAULT_READ_BROWSING_BUFFER_PERCENT,
            total_cache_budget_mib: rgv_core::DEFAULT_CACHE_BUDGET_MIB,
            alignment_scene_budget_bytes: 0,
            alignment_scene_charged_bytes: 0,
            alignment_scene_reserved_bytes: 0,
            alignment_scene_transition_reserve_bytes: 0,
            alignment_scene_transition_reserved_bytes: 0,
            alignment_scene_transient_budget_bytes: 0,
            alignment_scene_transient_bytes: 0,
            alignment_scene_transient_high_water_bytes: 0,
            alignment_scene_pinned_bytes: 0,
            alignment_scene_pinned_overage_bytes: 0,
            alignment_scene_active_replacement_bundle: None,
            alignment_scene_budget_refreshes: 0,
            alignment_scene_diagnostics: AutomationAlignmentSceneDiagnostics::default(),
            read_pane_height: 360.0,
            annotation_section_expanded: true,
            read_section_expanded: true,
            presentation_groups: Vec::new(),
            presentation_composites: Vec::new(),
            presentation_nodes: Vec::new(),
            pane_layouts: Vec::new(),
            workspace_layout: Some(AutomationWorkspaceLayoutSnapshot {
                revision: 3,
                recomputations: 2,
                settled_resize_query_refreshes: 1,
                resolution_query_refresh_pending: false,
                workspace: AutomationRectSnapshot {
                    left: 0.0,
                    top: 62.0,
                    right: 1_280.0,
                    bottom: 776.0,
                },
                properties: AutomationRectSnapshot {
                    left: 1_280.0,
                    top: 62.0,
                    right: 1_620.0,
                    bottom: 776.0,
                },
                properties_expanded: true,
                properties_resize_active: false,
                display_scale: 2.0,
                accessibility_scale: 1.0,
                genome_context: AutomationRectSnapshot {
                    left: 0.0,
                    top: 62.0,
                    right: 1_280.0,
                    bottom: 129.0,
                },
                track_viewport: AutomationRectSnapshot {
                    left: 0.0,
                    top: 129.0,
                    right: 1_280.0,
                    bottom: 776.0,
                },
                context_gutter: AutomationRectSnapshot {
                    left: 0.0,
                    top: 62.0,
                    right: 168.0,
                    bottom: 129.0,
                },
                track_gutter: AutomationRectSnapshot {
                    left: 0.0,
                    top: 129.0,
                    right: 168.0,
                    bottom: 776.0,
                },
                has_cytoband_row: false,
                panels: vec![AutomationWorkspacePanelLayoutSnapshot {
                    panel_id: PanelId::new(1),
                    cytobands: AutomationCytobandAvailability::Absent,
                    column: AutomationRectSnapshot {
                        left: 168.0,
                        top: 62.0,
                        right: 1_280.0,
                        bottom: 776.0,
                    },
                    header: AutomationRectSnapshot {
                        left: 168.0,
                        top: 62.0,
                        right: 1_280.0,
                        bottom: 129.0,
                    },
                    header_genomic: AutomationRectSnapshot {
                        left: 168.0,
                        top: 62.0,
                        right: 1_258.0,
                        bottom: 129.0,
                    },
                    header_scrollbar: AutomationRectSnapshot {
                        left: 1_258.0,
                        top: 62.0,
                        right: 1_280.0,
                        bottom: 129.0,
                    },
                    canvas: AutomationRectSnapshot {
                        left: 168.0,
                        top: 129.0,
                        right: 1_258.0,
                        bottom: 776.0,
                    },
                    scrollbar: AutomationRectSnapshot {
                        left: 1_258.0,
                        top: 129.0,
                        right: 1_280.0,
                        bottom: 776.0,
                    },
                    metadata_row: AutomationRectSnapshot::default(),
                    cytoband_row: None,
                    overview_row: AutomationRectSnapshot::default(),
                    span_row: AutomationRectSnapshot::default(),
                    ruler_row: AutomationRectSnapshot::default(),
                }],
            }),
            active_track_gesture: None,
            selection: Some(selection.clone()),
            status: Vec::new(),
            truncated: false,
        };

        let workspace_json = serde_json::to_string(&workspace).unwrap();
        assert_eq!(
            serde_json::from_str::<AutomationWorkspaceSnapshot>(&workspace_json).unwrap(),
            workspace
        );
        let selection_json = serde_json::to_string(&selection).unwrap();
        assert_eq!(
            serde_json::from_str::<AutomationSelectionSnapshot>(&selection_json).unwrap(),
            selection
        );
        // Safe semantic lane metadata may include a genomic source locus; the privacy boundary
        // forbids file source/path/sequence payload fields.
        assert!(!workspace_json.contains("\"source\":"));
        assert!(!workspace_json.contains("\"path\":"));
        assert!(!workspace_json.contains("\"sequence\":"));
    }

    #[test]
    fn version_one_reports_without_telemetry_receive_safe_defaults() {
        let report: AutomationRunReport = serde_json::from_str(
            r#"{
                "schema_version": 1,
                "succeeded": true,
                "results": [{
                    "command_index": 0,
                    "status": "succeeded",
                    "message": "done",
                    "data": null
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(report.telemetry, AutomationRunTelemetry::default());
        assert_eq!(report.results[0].command, "");
        assert!(report.results[0].elapsed_ms.abs() < f64::EPSILON);
    }

    #[test]
    fn future_versions_invalid_ids_and_unbounded_timeouts_are_rejected() {
        let future = r#"{"schema_version":99,"commands":[{"command":"exit"}]}"#;
        assert!(matches!(
            AutomationBatch::from_json(future),
            Err(AutomationError::UnsupportedVersion(99))
        ));

        let invalid = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::WaitUntilSettled {
                timeout_millis: MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS + 1,
            }],
        };
        assert!(matches!(
            invalid.to_json(),
            Err(AutomationError::InvalidCommand { index: 0, .. })
        ));
    }

    fn single_command_batch(command: AutomationCommand) -> AutomationBatch {
        AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![command],
        }
    }

    #[test]
    fn feature_query_and_gene_navigation_round_trip_with_defaults() {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::QueryFeatures {
                    contig: "chr17".to_owned(),
                    start: 43_044_294,
                    end: 43_125_483,
                    track_ids: Some(vec![TrackId::new(3), TrackId::new(5)]),
                    offset: 500,
                    limit: MAXIMUM_AUTOMATION_FEATURE_RESULTS,
                },
                AutomationCommand::GotoGene {
                    query: "BRCA1".to_owned(),
                    panel_id: Some(PanelId::new(2)),
                    flank_bases: 5_000,
                },
            ],
        };
        let json = batch.to_json().unwrap();
        assert_eq!(AutomationBatch::from_json(&json).unwrap(), batch);
        assert_eq!(
            batch
                .commands
                .iter()
                .map(AutomationCommand::name)
                .collect::<Vec<_>>(),
            ["query_features", "goto_gene"]
        );

        // Optional fields are additive: a minimal command receives the documented defaults.
        let minimal = AutomationBatch::from_json(
            r#"{"schema_version":6,"commands":[
                {"command":"query_features","contig":"chr1","start":0,"end":1000},
                {"command":"goto_gene","query":"TP53"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            minimal.commands,
            [
                AutomationCommand::QueryFeatures {
                    contig: "chr1".to_owned(),
                    start: 0,
                    end: 1_000,
                    track_ids: None,
                    offset: 0,
                    limit: 100,
                },
                AutomationCommand::GotoGene {
                    query: "TP53".to_owned(),
                    panel_id: None,
                    flank_bases: 0,
                },
            ]
        );

        let result = AutomationFeatureQueryResult {
            contig: "chr1".to_owned(),
            start: 0,
            end: 1_000,
            track_ids: vec![3],
            features: vec![AutomationFeature {
                track_id: 3,
                name: "TP53".to_owned(),
                feature_type: "gene".to_owned(),
                contig: "chr1".to_owned(),
                start: 10,
                end: 90,
                strand: "-".to_owned(),
                gene_name: None,
            }],
            total_matching: 4,
            offset: 0,
            truncated: true,
        };
        let value = serde_json::to_value(&result).unwrap();
        assert!(value["features"][0].get("gene_name").is_none());
        assert_eq!(
            serde_json::from_value::<AutomationFeatureQueryResult>(value).unwrap(),
            result
        );
        let navigation = AutomationGeneNavigationResult {
            panel_id: 2,
            contig: "chr17".to_owned(),
            start: 43_039_294,
            end: 43_130_483,
            hit: AutomationGeneHit {
                track_id: 3,
                name: "BRCA1".to_owned(),
                feature_type: "gene".to_owned(),
            },
            candidate_count: 1,
        };
        // The viewer adds `application_instance_id` and `workspace_revision` to every result.
        let mut value = serde_json::to_value(&navigation).unwrap();
        value["workspace_revision"] = serde_json::json!(9);
        assert_eq!(
            serde_json::from_value::<AutomationGeneNavigationResult>(value).unwrap(),
            navigation
        );
        assert_eq!(
            serde_json::to_value(AutomationErrorCode::AmbiguousMatch).unwrap(),
            "ambiguous_match"
        );
    }

    #[test]
    fn feature_queries_reject_empty_unbounded_or_inverted_requests() {
        let query = |contig: &str, start, end, track_ids, offset, limit| {
            single_command_batch(AutomationCommand::QueryFeatures {
                contig: contig.to_owned(),
                start,
                end,
                track_ids,
                offset,
                limit,
            })
        };
        let widest = MAXIMUM_AUTOMATION_FEATURE_QUERY_SPAN;
        for accepted in [
            query("chr1", 0, 1, None, 0, 1),
            query(
                "chr1",
                7,
                7 + widest,
                Some(vec![TrackId::new(1)]),
                MAXIMUM_AUTOMATION_FEATURE_OFFSET,
                MAXIMUM_AUTOMATION_FEATURE_RESULTS,
            ),
            query("chr1", u64::MAX - 1, u64::MAX, None, 0, 100),
        ] {
            accepted.validate().unwrap();
        }
        for rejected in [
            query("", 0, 10, None, 0, 100),
            query("  ", 0, 10, None, 0, 100),
            query(
                &"c".repeat(MAXIMUM_AUTOMATION_STRING_BYTES + 1),
                0,
                10,
                None,
                0,
                100,
            ),
            query("chr1", 10, 10, None, 0, 100),
            query("chr1", 11, 10, None, 0, 100),
            query("chr1", 0, widest + 1, None, 0, 100),
            query("chr1", 0, 10, Some(Vec::new()), 0, 100),
            query("chr1", 0, 10, Some(vec![TrackId::new(0)]), 0, 100),
            query(
                "chr1",
                0,
                10,
                Some(vec![
                    TrackId::new(1);
                    MAXIMUM_AUTOMATION_TRACK_SELECTION + 1
                ]),
                0,
                100,
            ),
            query(
                "chr1",
                0,
                10,
                None,
                MAXIMUM_AUTOMATION_FEATURE_OFFSET + 1,
                100,
            ),
            query("chr1", 0, 10, None, 0, 0),
            query(
                "chr1",
                0,
                10,
                None,
                0,
                MAXIMUM_AUTOMATION_FEATURE_RESULTS + 1,
            ),
        ] {
            assert!(
                matches!(
                    rejected.validate(),
                    Err(AutomationError::InvalidCommand { index: 0, .. })
                ),
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn gene_navigation_rejects_empty_or_unbounded_requests() {
        let goto = |query: &str, panel_id, flank_bases| {
            single_command_batch(AutomationCommand::GotoGene {
                query: query.to_owned(),
                panel_id,
                flank_bases,
            })
        };
        for accepted in [
            goto("BRCA1", None, 0),
            goto(
                &"g".repeat(MAXIMUM_AUTOMATION_GENE_QUERY_BYTES),
                Some(PanelId::new(1)),
                MAXIMUM_AUTOMATION_GENE_FLANK_BASES,
            ),
        ] {
            accepted.validate().unwrap();
        }
        for rejected in [
            goto("", None, 0),
            goto(" \t", None, 0),
            goto(
                &"g".repeat(MAXIMUM_AUTOMATION_GENE_QUERY_BYTES + 1),
                None,
                0,
            ),
            goto("BRCA1", Some(PanelId::new(0)), 0),
            goto("BRCA1", None, MAXIMUM_AUTOMATION_GENE_FLANK_BASES + 1),
        ] {
            assert!(
                matches!(
                    rejected.validate(),
                    Err(AutomationError::InvalidCommand { index: 0, .. })
                ),
                "{rejected:?}"
            );
        }
    }

    #[test]
    fn typed_alignment_color_tags_enforce_sam_tag_syntax() {
        let invalid = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Configure {
                setting: AutomationSetting::AlignmentColorMode {
                    track_ids: vec![TrackId::new(1)],
                    value: AlignmentColorMode::Tag(*b"1!"),
                },
            }],
        };
        assert!(matches!(
            invalid.to_json(),
            Err(AutomationError::InvalidCommand { index: 0, .. })
        ));
    }

    #[test]
    fn stable_mode_and_one_shot_repack_are_distinct_typed_settings() {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::Configure {
                    setting: AutomationSetting::ReadStackingMode {
                        track_ids: vec![TrackId::new(2)],
                        value: ReadStackingMode::Stable,
                    },
                },
                AutomationCommand::Configure {
                    setting: AutomationSetting::RebuildStableReadArchitecture {
                        panel_id: PanelId::new(1),
                        track_ids: vec![TrackId::new(2)],
                    },
                },
            ],
        };

        let json = batch.to_json().unwrap();
        assert!(json.contains("read_stacking_mode"));
        assert!(json.contains("rebuild_stable_read_architecture"));
        assert_eq!(AutomationBatch::from_json(&json).unwrap(), batch);
        let legacy = json.replace("rebuild_stable_read_architecture", "compact_repack_reads");
        assert_eq!(AutomationBatch::from_json(&legacy).unwrap(), batch);
    }

    #[test]
    fn complete_read_limit_is_a_bounded_typed_setting() {
        let valid = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Configure {
                setting: AutomationSetting::CompleteReadLimit {
                    track_ids: vec![TrackId::new(2)],
                    value: MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT,
                },
            }],
        };
        let json = valid.to_json().unwrap();
        assert!(json.contains("complete_read_limit"));
        assert_eq!(AutomationBatch::from_json(&json).unwrap(), valid);

        for value in [999, MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT + 1] {
            let invalid = AutomationBatch {
                schema_version: AUTOMATION_SCHEMA_VERSION,
                commands: vec![AutomationCommand::Configure {
                    setting: AutomationSetting::CompleteReadLimit {
                        track_ids: vec![TrackId::new(2)],
                        value,
                    },
                }],
            };
            assert!(matches!(
                invalid.to_json(),
                Err(AutomationError::InvalidCommand { index: 0, .. })
            ));
        }
    }

    #[test]
    fn properties_panel_automation_width_is_bounded() {
        let invalid = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Configure {
                setting: AutomationSetting::PropertiesPanelWidth { value: 279.0 },
            }],
        };
        assert!(matches!(
            invalid.to_json(),
            Err(AutomationError::InvalidCommand { index: 0, .. })
        ));

        let valid = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::Configure {
                    setting: AutomationSetting::WindowSize {
                        width: 800,
                        height: 600,
                    },
                },
                AutomationCommand::Configure {
                    setting: AutomationSetting::InterfaceScalePercent { value: 150 },
                },
                AutomationCommand::Configure {
                    setting: AutomationSetting::ReadBrowsingBufferPercent { value: 200 },
                },
                AutomationCommand::Configure {
                    setting: AutomationSetting::TotalCacheBudgetMib { value: 2_048 },
                },
                AutomationCommand::Configure {
                    setting: AutomationSetting::PropertiesPanelExpanded { value: false },
                },
                AutomationCommand::Configure {
                    setting: AutomationSetting::PropertiesPanelWidth { value: 520.0 },
                },
                AutomationCommand::Configure {
                    setting: AutomationSetting::PropertiesPanelResizeActive { value: true },
                },
            ],
        };
        assert!(valid.to_json().is_ok());

        let invalid_buffer = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Configure {
                setting: AutomationSetting::ReadBrowsingBufferPercent {
                    value: rgv_core::MAXIMUM_READ_BROWSING_BUFFER_PERCENT + 1,
                },
            }],
        };
        assert!(matches!(
            invalid_buffer.to_json(),
            Err(AutomationError::InvalidCommand { index: 0, .. })
        ));
        let invalid_cache = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Configure {
                setting: AutomationSetting::TotalCacheBudgetMib {
                    value: rgv_core::MAXIMUM_CACHE_BUDGET_MIB + 1,
                },
            }],
        };
        assert!(matches!(
            invalid_cache.to_json(),
            Err(AutomationError::InvalidCommand { index: 0, .. })
        ));
    }

    #[test]
    fn persisted_batch_sources_never_retain_remote_credentials() {
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::LoadTrack {
                source: TrackSource::Remote {
                    url: "https://user:secret@example.org/reads.bam?token=private".to_owned(),
                },
                kind: Some(TrackKind::Alignment),
                name: None,
            }],
        };
        let json = batch.to_json().unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("token"));
        assert!(json.contains("https://example.org/reads.bam"));
    }

    #[test]
    fn reading_a_batch_roots_every_local_input_and_output_beside_the_batch() {
        let directory = tempfile::tempdir().unwrap();
        let batch_path = directory.path().join("review.json");
        let batch = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::SetGenome {
                    source: TrackSource::Local {
                        path: "data/reference.fa".to_owned(),
                    },
                    name: None,
                },
                AutomationCommand::LoadTrack {
                    source: TrackSource::Local {
                        path: "data/reads.bam".to_owned(),
                    },
                    kind: Some(TrackKind::Alignment),
                    name: None,
                },
                AutomationCommand::Snapshot {
                    path: "../results/view.svg".to_owned(),
                    scope: AutomationSnapshotScope::VisibleWorkspace,
                    format: AutomationSnapshotFormat::Svg,
                    scale: default_snapshot_scale(),
                },
                AutomationCommand::Save {
                    path: "results/project.json".to_owned(),
                },
            ],
        };
        std::fs::write(&batch_path, batch.to_json().unwrap()).unwrap();

        let loaded = AutomationBatch::read(&batch_path).unwrap();
        let root = std::path::absolute(directory.path()).unwrap();
        assert!(matches!(
            &loaded.commands[0],
            AutomationCommand::SetGenome {
                source: TrackSource::Local { path },
                ..
            } if Path::new(path) == root.join("data/reference.fa")
        ));
        assert!(matches!(
            &loaded.commands[1],
            AutomationCommand::LoadTrack {
                source: TrackSource::Local { path },
                ..
            } if Path::new(path) == root.join("data/reads.bam")
        ));
        assert!(matches!(
            &loaded.commands[2],
            AutomationCommand::Snapshot { path, .. }
                if Path::new(path) == root.parent().unwrap().join("results/view.svg")
        ));
        assert!(matches!(
            &loaded.commands[3],
            AutomationCommand::Save { path }
                if Path::new(path) == root.join("results/project.json")
        ));
    }

    #[test]
    fn local_control_paths_cannot_escape_the_selected_root() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("allowed");
        std::fs::create_dir(&root).unwrap();
        let mut accepted = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Snapshot {
                path: "results/view.svg".to_owned(),
                scope: AutomationSnapshotScope::VisibleWorkspace,
                format: AutomationSnapshotFormat::Svg,
                scale: default_snapshot_scale(),
            }],
        };
        accepted.resolve_paths_within(&root).unwrap();
        assert!(matches!(
            &accepted.commands[0],
            AutomationCommand::Snapshot { path, .. }
                if Path::new(path) == std::path::absolute(&root).unwrap().join("results/view.svg")
        ));

        let mut traversal = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Save {
                path: "../outside.json".to_owned(),
            }],
        };
        assert!(matches!(
            traversal.resolve_paths_within(&root),
            Err(AutomationError::PathOutsideControlRoot(_))
        ));

        let mut absolute = AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::LoadTrack {
                source: TrackSource::Local {
                    path: directory.path().join("private.bam").display().to_string(),
                },
                kind: Some(TrackKind::Alignment),
                name: None,
            }],
        };
        assert!(matches!(
            absolute.resolve_paths_within(&root),
            Err(AutomationError::PathOutsideControlRoot(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn local_control_paths_cannot_escape_the_root_through_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("allowed");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::create_dir(root.join("inner")).unwrap();
        std::fs::write(outside.join("private.bam"), b"secret").unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        symlink(outside.join("private.bam"), root.join("linked.bam")).unwrap();
        symlink(outside.join("not-yet.json"), root.join("dangling.json")).unwrap();
        symlink(root.join("inner"), root.join("inside-link")).unwrap();
        let through_link = directory.path().join("root-link");
        symlink(&root, &through_link).unwrap();

        let read = |path: &str| AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::LoadTrack {
                source: TrackSource::Local {
                    path: path.to_owned(),
                },
                kind: Some(TrackKind::Alignment),
                name: None,
            }],
        };
        let write = |path: &str| AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![AutomationCommand::Save {
                path: path.to_owned(),
            }],
        };

        // Reads and writes through a link that leaves the root are refused,
        // including outputs that do not exist yet and dangling links.
        for mut escaping in [
            read("escape/private.bam"),
            read("linked.bam"),
            write("escape/new/project.json"),
            write("escape/../escape/project.json"),
            write("dangling.json"),
            write("linked.bam"),
        ] {
            assert!(
                matches!(
                    escaping.resolve_paths_within(&root),
                    Err(AutomationError::PathOutsideControlRoot(_))
                ),
                "{:?}",
                escaping.commands[0]
            );
        }

        // Links that stay inside the root, missing outputs, and a root that is
        // itself reached through a link all remain usable.
        for mut contained in [
            write("inside-link/project.json"),
            write("new/deeper/project.json"),
            read("inner/missing.bam"),
        ] {
            contained.resolve_paths_within(&root).unwrap();
            contained.resolve_paths_within(&through_link).unwrap();
        }
        let mut stored = write("inside-link/project.json");
        stored.resolve_paths_within(&through_link).unwrap();
        assert!(matches!(
            &stored.commands[0],
            AutomationCommand::Save { path }
                if Path::new(path) == through_link.join("inside-link/project.json")
        ));
        let mut absolute = write(&root.join("inner/absolute.json").display().to_string());
        absolute.resolve_paths_within(&through_link).unwrap();
        let mut escaping = read(&outside.join("private.bam").display().to_string());
        assert!(escaping.resolve_paths_within(&through_link).is_err());
    }

    #[test]
    fn checked_in_desktop_smoke_batch_matches_the_public_schema() {
        let batch =
            AutomationBatch::from_json(include_str!("../../../validation/automation-smoke.json"))
                .unwrap();
        assert_eq!(batch.commands.len(), 19);
        assert!(matches!(
            batch.commands.last(),
            Some(AutomationCommand::Exit)
        ));
    }

    #[test]
    fn automation_remote_urls_are_http_only_and_not_private_network_literals() {
        for accepted in [
            "https://example.org/reads.bam",
            "http://data.example.org:8080/a.bw",
            "https://8.8.8.8/x.bam",
            "https://[2001:db8::1]/x.bam",
            "https://100.128.0.1/x.bam",
        ] {
            assert!(
                check_automation_remote_url(accepted, false).is_ok(),
                "{accepted}"
            );
        }
        for scheme in [
            "file:///etc/passwd",
            "ftp://example.org/a.bam",
            "s3://bucket/a.bam",
            "gopher://example.org/",
            "not a url",
        ] {
            // The scheme rule holds even when private addresses are allowed.
            assert!(
                check_automation_remote_url(scheme, false).is_err(),
                "{scheme}"
            );
            assert!(
                check_automation_remote_url(scheme, true).is_err(),
                "{scheme}"
            );
        }
        for private in [
            "http://localhost:9000/a.bam",
            "http://LOCALHOST./a.bam",
            "http://api.localhost/a.bam",
            "http://127.0.0.1/a.bam",
            "http://127.8.9.10/a.bam",
            "http://10.1.2.3/a.bam",
            "http://172.16.0.1/a.bam",
            "http://192.168.1.1/a.bam",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/a.bam",
            "http://0.0.0.0/a.bam",
            "http://[::1]/a.bam",
            "http://[fe80::1]/a.bam",
            "http://[fd00::1]/a.bam",
            "http://[::ffff:127.0.0.1]/a.bam",
            "http://[::ffff:10.0.0.1]/a.bam",
            // Alternative spellings of 127.0.0.1 that URL parsing canonicalises.
            "http://2130706433/a.bam",
            "http://0x7f.1/a.bam",
        ] {
            assert!(
                matches!(
                    check_automation_remote_url(private, false),
                    Err(AutomationError::RemoteSourceRefused { .. })
                ),
                "{private}"
            );
            assert!(
                check_automation_remote_url(private, true).is_ok(),
                "{private}"
            );
        }
        // Credentials never reach the error text.
        let error = check_automation_remote_url("http://user:hunter2@127.0.0.1/a.bam", false)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("hunter2"), "{error}");
    }

    #[test]
    fn control_batches_cannot_load_refused_remote_sources() {
        let batch = |url: &str| AutomationBatch {
            schema_version: AUTOMATION_SCHEMA_VERSION,
            commands: vec![
                AutomationCommand::DescribeWorkspace,
                AutomationCommand::LoadTrack {
                    source: TrackSource::Remote {
                        url: url.to_owned(),
                    },
                    kind: Some(TrackKind::Alignment),
                    name: None,
                },
            ],
        };
        assert!(
            batch("https://example.org/a.bam")
                .check_remote_sources(false)
                .is_ok()
        );
        assert!(
            batch("http://127.0.0.1:8000/a.bam")
                .check_remote_sources(false)
                .is_err()
        );
        assert!(
            batch("http://127.0.0.1:8000/a.bam")
                .check_remote_sources(true)
                .is_ok()
        );
        assert!(
            batch("file:///tmp/a.bam")
                .check_remote_sources(true)
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn sources_named_by_a_control_opened_project_obey_the_control_root() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("allowed");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(root.join("reads.bam"), b"inside").unwrap();
        std::fs::write(outside.join("private.bam"), b"secret").unwrap();
        symlink(outside.join("private.bam"), root.join("linked.bam")).unwrap();

        let project = |source: TrackSource| {
            let mut document = rgv_core::SessionDocument::new(rgv_core::SessionLocus {
                contig: "chr1".to_owned(),
                start: 0,
                end: 100,
            });
            document.reference = Some(source);
            document
        };
        let local = |path: &Path| TrackSource::Local {
            path: path.display().to_string(),
        };
        let remote = |url: &str| TrackSource::Remote {
            url: url.to_owned(),
        };
        let check = |document: &rgv_core::SessionDocument| {
            check_session_sources_within(document, &root, false)
        };

        assert!(check(&project(local(&root.join("reads.bam")))).is_ok());
        // A source that does not exist yet is judged by where it would be.
        assert!(check(&project(local(&root.join("missing.bam")))).is_ok());
        assert!(check(&project(remote("https://example.org/ref.fa"))).is_ok());
        for refused in [
            local(&outside.join("private.bam")),
            // Inside the root by name, outside it in fact.
            local(&root.join("linked.bam")),
            local(&root.join("../outside/private.bam")),
        ] {
            assert!(
                matches!(
                    check(&project(refused.clone())),
                    Err(AutomationError::PathOutsideControlRoot(_))
                ),
                "{refused:?}"
            );
        }
        assert!(matches!(
            check(&project(remote("http://169.254.169.254/latest/"))),
            Err(AutomationError::RemoteSourceRefused { .. })
        ));

        // Every source-bearing field is covered, not only the reference.
        let mut document = project(local(&root.join("reads.bam")));
        document.cytobands = Some(local(&outside.join("private.bam")));
        assert!(check(&document).is_err());
        let mut document = project(local(&root.join("reads.bam")));
        document.remote_reference_index_url = Some("http://10.0.0.1/ref.fa.fai".to_owned());
        assert!(check(&document).is_err());
        assert!(check_session_sources_within(&document, &root, true).is_ok());
    }

    #[test]
    fn snapshot_scale_is_additive_defaulted_and_bounded() {
        // A command written before the field existed still parses, at one pixel per point.
        let legacy: AutomationCommand =
            serde_json::from_str(r#"{"command":"snapshot","path":"view.png"}"#).unwrap();
        let AutomationCommand::Snapshot { scale, .. } = &legacy else {
            panic!("expected a snapshot command");
        };
        assert_eq!(scale.to_bits(), 1.0_f32.to_bits());
        assert!(legacy.validate().is_ok());
        // The default is not serialized, so the wire form of existing batches is unchanged.
        assert_eq!(
            serde_json::to_value(&legacy).unwrap(),
            serde_json::json!({
                "command": "snapshot", "path": "view.png",
                "scope": "visible_workspace", "format": "png"
            })
        );

        let scaled: AutomationCommand =
            serde_json::from_str(r#"{"command":"snapshot","path":"view.png","scale":2.0}"#)
                .unwrap();
        assert!(scaled.validate().is_ok());
        assert_eq!(serde_json::to_value(&scaled).unwrap()["scale"], 2.0);

        for scale in [0.0, -1.0, 0.1, 8.5, f32::NAN, f32::INFINITY] {
            let command = AutomationCommand::Snapshot {
                path: "view.png".to_owned(),
                scope: AutomationSnapshotScope::VisibleWorkspace,
                format: AutomationSnapshotFormat::Png,
                scale,
            };
            assert!(command.validate().is_err(), "{scale}");
        }
        let native = AutomationCommand::Snapshot {
            path: "window.png".to_owned(),
            scope: AutomationSnapshotScope::NativeWindow,
            format: AutomationSnapshotFormat::Png,
            scale: 2.0,
        };
        assert!(native.validate().is_err());
    }

    #[test]
    fn the_track_pipeline_setting_round_trips_and_rejects_unknown_pipelines() {
        let wire = serde_json::json!({ "setting": "track_pipeline", "value": "tiles" });
        let setting: AutomationSetting = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(
            setting,
            AutomationSetting::TrackPipeline {
                value: rgv_core::TrackPipeline::Tiles
            }
        );
        assert!(setting.validate().is_ok());
        assert_eq!(serde_json::to_value(&setting).unwrap(), wire);
        let legacy: AutomationSetting = serde_json::from_value(
            serde_json::json!({ "setting": "track_pipeline", "value": "legacy" }),
        )
        .unwrap();
        assert_eq!(
            legacy,
            AutomationSetting::TrackPipeline {
                value: rgv_core::TrackPipeline::Legacy
            }
        );
        assert!(
            serde_json::from_value::<AutomationSetting>(
                serde_json::json!({ "setting": "track_pipeline", "value": "fast" })
            )
            .is_err()
        );
    }

    #[test]
    fn per_track_settings_accept_an_optional_track_and_keep_the_unnamed_wire_form() {
        let unnamed = serde_json::json!({ "setting": "show_reads", "value": false });
        let setting: AutomationSetting = serde_json::from_value(unnamed.clone()).unwrap();
        assert_eq!(
            setting,
            AutomationSetting::ShowReads {
                value: false,
                track_id: None
            }
        );
        assert_eq!(serde_json::to_value(&setting).unwrap(), unnamed);

        let named: AutomationSetting = serde_json::from_value(serde_json::json!({
            "setting": "vertical_scroll_points", "panel_id": 2, "track_id": 5, "value": 40.0
        }))
        .unwrap();
        assert_eq!(
            named,
            AutomationSetting::VerticalScrollPoints {
                panel_id: PanelId::new(2),
                track_id: Some(TrackId::new(5)),
                value: 40.0,
            }
        );
        assert!(named.validate().is_ok());

        let density: AutomationSetting = serde_json::from_value(serde_json::json!({
            "setting": "alignment_density", "track_id": 3, "value": 1.5
        }))
        .unwrap();
        assert!(density.validate().is_ok());
        for zero in [
            serde_json::json!({ "setting": "show_reads", "track_id": 0, "value": true }),
            serde_json::json!({ "setting": "alignment_density", "track_id": 0, "value": 1.0 }),
            serde_json::json!({
                "setting": "vertical_scroll_points", "panel_id": 1, "track_id": 0, "value": 0.0
            }),
        ] {
            let setting: AutomationSetting = serde_json::from_value(zero).unwrap();
            assert!(setting.validate().is_err());
        }
    }
}
