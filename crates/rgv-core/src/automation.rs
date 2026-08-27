use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
    CoverageScaleTransform, DetailLevel, ModifiedBaseColorMode, OverlappingMatePolicy, PanelId,
    ReadBaseDisplayMode, ReviewStatus, TrackId, TrackKind, TrackSource,
};

pub const AUTOMATION_SCHEMA_VERSION: u32 = 4;
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
    /// rejects lexical attempts to access paths outside it.
    pub fn resolve_paths_within(&mut self, root: impl AsRef<Path>) -> Result<(), AutomationError> {
        let root = normalize_automation_path(&std::path::absolute(root)?);
        for command in &mut self.commands {
            command.resolve_paths_within(&root)?;
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
#[serde(tag = "command", rename_all = "snake_case")]
pub enum AutomationCommand {
    DescribeWorkspace,
    DescribeSelection,
    DescribeInteractionTrace,
    ClearSelection,
    RefreshSelection,
    DescribeReviewQueue,
    DescribeNavigationHistory {
        panel_id: PanelId,
    },
    NewWorkspace,
    OpenWorkspace {
        path: String,
        confirm_replace: bool,
        #[serde(default)]
        unsaved_changes: AutomationUnsavedChangesPolicy,
        #[serde(default = "default_settle_timeout_millis")]
        timeout_millis: u64,
    },
    SetGenome {
        source: TrackSource,
        #[serde(default)]
        name: Option<String>,
    },
    LoadTrack {
        source: TrackSource,
        #[serde(default)]
        kind: Option<TrackKind>,
        #[serde(default)]
        name: Option<String>,
    },
    RemoveTrack {
        track_id: TrackId,
    },
    Goto {
        loci: Vec<String>,
    },
    ChangeViewport {
        panel_id: PanelId,
        operation: AutomationViewportOperation,
    },
    AddPanel {
        locus: String,
    },
    ClosePanel {
        panel_id: PanelId,
    },
    SetOption {
        scope: AutomationOptionScope,
        key: String,
        value: serde_json::Value,
    },
    Configure {
        setting: AutomationSetting,
    },
    Group {
        track_ids: Vec<TrackId>,
        mode: AlignmentGroupMode,
        #[serde(default)]
        direction: AlignmentOrderDirection,
        #[serde(default)]
        tag: Option<String>,
    },
    Sort {
        track_ids: Vec<TrackId>,
        mode: AlignmentSortMode,
        #[serde(default)]
        direction: AlignmentOrderDirection,
        #[serde(default)]
        tag: Option<String>,
    },
    Filter {
        track_ids: Vec<TrackId>,
        expression: String,
    },
    Select {
        selection: AutomationSelection,
    },
    FindAlignments {
        panel_id: PanelId,
        track_id: TrackId,
        selector: AutomationAlignmentSelector,
        #[serde(default = "default_alignment_result_limit")]
        maximum_results: usize,
    },
    SelectAlignment {
        handle: String,
        #[serde(default)]
        reveal: AutomationAlignmentReveal,
        #[serde(default)]
        expected_workspace_revision: Option<u64>,
    },
    DescribeRelatedAlignments,
    NavigateRelatedAlignment {
        relationship: AutomationAlignmentRelationship,
        #[serde(default)]
        segment_index: Option<usize>,
        #[serde(default)]
        destination: AutomationNavigationDestination,
        #[serde(default)]
        expected_workspace_revision: Option<u64>,
    },
    NavigateReviewItem {
        action: AutomationReviewNavigation,
    },
    NavigateHistory {
        panel_id: PanelId,
        action: AutomationHistoryNavigation,
    },
    WaitUntilSettled {
        #[serde(default = "default_settle_timeout_millis")]
        timeout_millis: u64,
    },
    Snapshot {
        path: String,
        #[serde(default)]
        scope: AutomationSnapshotScope,
        #[serde(default)]
        format: AutomationSnapshotFormat,
    },
    Export {
        path: String,
        kind: AutomationExportKind,
    },
    Save {
        path: String,
    },
    Exit,
}

impl AutomationCommand {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::DescribeWorkspace => "describe_workspace",
            Self::DescribeSelection => "describe_selection",
            Self::DescribeInteractionTrace => "describe_interaction_trace",
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
            | Self::ClearSelection
            | Self::RefreshSelection
            | Self::DescribeReviewQueue
            | Self::DescribeRelatedAlignments
            | Self::NewWorkspace
            | Self::Exit => Ok(()),
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
            Self::Group { track_ids, tag, .. } | Self::Sort { track_ids, tag, .. } => {
                validate_track_selection(track_ids)?;
                validate_optional_sam_tag(tag.as_deref())
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
            Self::WaitUntilSettled { timeout_millis } => validate_settle_timeout(*timeout_millis),
            Self::Snapshot { path, .. } | Self::Export { path, .. } | Self::Save { path } => {
                validate_string(path, "output path")
            }
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

    fn resolve_paths_within(&mut self, root: &Path) -> Result<(), AutomationError> {
        let resolve = |value: &mut String| -> Result<(), AutomationError> {
            let path = Path::new(value);
            let candidate = if path.is_relative() {
                root.join(path)
            } else {
                path.to_owned()
            };
            let resolved = normalize_automation_path(&candidate);
            if !resolved.starts_with(root) {
                return Err(AutomationError::PathOutsideControlRoot(
                    path.display().to_string(),
                ));
            }
            *value = resolved.display().to_string();
            Ok(())
        };
        match self {
            Self::SetGenome { source, .. } | Self::LoadTrack { source, .. } => {
                if let TrackSource::Local { path } = source {
                    resolve(path)?;
                }
            }
            Self::OpenWorkspace { path, .. }
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
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum AutomationViewportOperation {
    PanBases {
        delta: i64,
    },
    PanFraction {
        fraction: f64,
    },
    Zoom {
        factor: f64,
        #[serde(default)]
        anchor_position: Option<u64>,
    },
    FitInterval {
        start: u64,
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
#[serde(rename_all = "snake_case")]
pub enum AutomationUnsavedChangesPolicy {
    #[default]
    Reject,
    SaveThenOpen,
    Discard,
}

/// Typed configuration accepted by automation schema v4. Each variant owns
/// its valid scope, preventing key/scope mismatches at the protocol boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "setting", rename_all = "snake_case")]
pub enum AutomationSetting {
    WindowSize {
        width: u32,
        height: u32,
    },
    InterfaceScalePercent {
        value: u16,
    },
    PropertiesPanelExpanded {
        value: bool,
    },
    PropertiesPanelWidth {
        value: f32,
    },
    /// Transient control signal matching a held Properties resize sash. This
    /// gesture state is never persisted in a project.
    PropertiesPanelResizeActive {
        value: bool,
    },
    ShowReference {
        value: bool,
    },
    ShowCoverage {
        value: bool,
    },
    ShowReads {
        value: bool,
    },
    ShowJunctions {
        value: bool,
    },
    ViewAsPairs {
        value: bool,
    },
    OverlappingMatePolicy {
        value: OverlappingMatePolicy,
    },
    AlignmentColorMode {
        track_ids: Vec<TrackId>,
        value: AlignmentColorMode,
    },
    AlignmentDensity {
        value: f32,
    },
    IncludeSecondary {
        value: bool,
    },
    IncludeSupplementary {
        value: bool,
    },
    CoverageScaleTransform {
        value: CoverageScaleTransform,
    },
    ReadBaseDisplayMode {
        value: ReadBaseDisplayMode,
    },
    ShadeReadBasesByQuality {
        value: bool,
    },
    ShowModifiedBases {
        value: bool,
    },
    ModifiedBaseColorMode {
        value: ModifiedBaseColorMode,
    },
    PresentationNodeVisible {
        target: crate::PresentationNodeId,
        value: bool,
    },
    PresentationNodeHeight {
        target: crate::PresentationNodeId,
        value: f32,
    },
    PresentationGroupExpanded {
        target: crate::PresentationNodeId,
        value: bool,
    },
    PresentationGroupVisible {
        target: crate::PresentationNodeId,
        value: bool,
    },
    PresentationCompositeVisible {
        channel: crate::TrackChannel,
        composite_id: u32,
        value: bool,
    },
    PresentationGroup {
        channel: crate::TrackChannel,
        track_ids: Vec<TrackId>,
    },
    PresentationUngroup {
        target: crate::PresentationNodeId,
    },
    ReadPaneHeight {
        value: f32,
    },
    TrackHeight {
        track_id: TrackId,
        value: f32,
    },
    TrackCollapsed {
        track_id: TrackId,
        value: bool,
    },
    TrackVisible {
        track_id: TrackId,
        value: bool,
    },
    VerticalScrollPoints {
        panel_id: PanelId,
        value: f32,
    },
    AnnotationScrollPoints {
        panel_id: PanelId,
        value: f32,
    },
    ReadStackScrollPoints {
        panel_id: PanelId,
        value: f32,
    },
    ReadPanelScrollPoints {
        panel_id: PanelId,
        track_id: TrackId,
        value: f32,
    },
    ReadPanelDensity {
        panel_id: PanelId,
        track_id: TrackId,
        value: f32,
    },
}

impl AutomationSetting {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::WindowSize { width, height } => ((640..=3_840).contains(width)
                && (400..=2_160).contains(height))
            .then_some(())
            .ok_or_else(|| {
                "window size must be between 640x400 and 3840x2160 logical points".to_owned()
            }),
            Self::InterfaceScalePercent { value } => (crate::MINIMUM_INTERFACE_SCALE_PERCENT
                ..=crate::MAXIMUM_INTERFACE_SCALE_PERCENT)
                .contains(value)
                .then_some(())
                .ok_or_else(|| "interface scale must be between 75 and 200 percent".to_owned()),
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
            Self::AlignmentDensity { value }
                if value.is_finite() && (0.25..=4.0).contains(value) =>
            {
                Ok(())
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
                crate::PresentationNodeId::Reference | crate::PresentationNodeId::Track { .. } => {
                    Ok(())
                }
                crate::PresentationNodeId::Group { .. } => Err(
                    "presentation group visibility must use presentation_group_visible".to_owned(),
                ),
            },
            Self::PresentationNodeHeight { target, value } => {
                if matches!(target, crate::PresentationNodeId::Group { .. }) {
                    return Err(
                        "presentation groups do not have a resizable content row".to_owned()
                    );
                }
                let valid = value.is_finite()
                    && match target.channel() {
                        crate::TrackChannel::Annotation => (26.0..=1_200.0).contains(value),
                        crate::TrackChannel::Reads => (40.0..=1_200.0).contains(value),
                    };
                valid.then_some(()).ok_or_else(|| {
                    "presentation node height is outside the valid range for its channel".to_owned()
                })
            }
            Self::PresentationGroupExpanded { target, .. }
            | Self::PresentationGroupVisible { target, .. }
            | Self::PresentationUngroup { target } => {
                matches!(target, crate::PresentationNodeId::Group { .. })
                    .then_some(())
                    .ok_or_else(|| "the setting target must be a presentation group".to_owned())
            }
            Self::PresentationCompositeVisible {
                channel,
                composite_id,
                ..
            } => (*channel == crate::TrackChannel::Annotation && *composite_id > 0)
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
            Self::VerticalScrollPoints { panel_id, value }
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
#[serde(tag = "selector", rename_all = "snake_case")]
pub enum AutomationAlignmentSelector {
    ExactReadName {
        read_name: String,
    },
    Position {
        position: u64,
    },
    Interval {
        start: u64,
        end: u64,
    },
    Semantic {
        #[serde(default)]
        alignment_state: Option<AutomationAlignmentState>,
        #[serde(default)]
        pair_class: Option<String>,
        #[serde(default)]
        has_mapped_mate: Option<bool>,
        #[serde(default)]
        has_supplementary_chain: Option<bool>,
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
#[serde(rename_all = "snake_case")]
pub enum AutomationAlignmentState {
    Primary,
    Secondary,
    Supplementary,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationAlignmentReveal {
    #[default]
    None,
    Center,
    Fit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationAlignmentRelationship {
    Mate,
    SupplementarySegment,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationNavigationDestination {
    #[default]
    CurrentPanel,
    NewPanel,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AutomationReviewNavigation {
    Item { review_item_id: u64 },
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
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AutomationHistoryNavigation {
    Back,
    Forward,
    Index { index: usize },
}

fn default_alignment_result_limit() -> usize {
    25
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
    #[serde(default)]
    pub read_pane_height: f32,
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
    pub channel: crate::TrackChannel,
    pub id: u32,
    pub label: String,
    pub members: Vec<TrackId>,
    pub expanded: bool,
    pub content_visible: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AutomationPresentationCompositeSnapshot {
    pub channel: crate::TrackChannel,
    pub id: u32,
    pub content_visible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationPresentationNodeSnapshot {
    pub id: crate::PresentationNodeId,
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
    pub id: crate::PresentationNodeId,
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
    pub channel: crate::TrackChannel,
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
    pub target: Option<crate::PresentationNodeId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationGenomeSnapshot {
    pub name: String,
    #[serde(default)]
    pub context_label: String,
    #[serde(default)]
    pub assembly_identity: Option<crate::GenomeAssemblyIdentity>,
    #[serde(default)]
    pub suggested_assembly_identity: Option<crate::GenomeAssemblyIdentity>,
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
    pub render_lanes: Vec<AutomationRenderLaneSnapshot>,
    #[serde(default)]
    pub read_panel_states: Vec<AutomationReadPanelStateSnapshot>,
    #[serde(default)]
    pub selected_presentation_nodes: Vec<crate::PresentationNodeId>,
    #[serde(default)]
    pub presentation_selection_anchor: Option<crate::PresentationNodeId>,
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationTrackSnapshot {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
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

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutomationAlignmentTrackSnapshot {
    pub group_mode: AlignmentGroupMode,
    pub group_order: AlignmentOrderDirection,
    pub sort_mode: AlignmentSortMode,
    pub sort_order: AlignmentOrderDirection,
    pub filter_expression: String,
    #[serde(default)]
    pub view_as_pairs: bool,
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
#[serde(rename_all = "snake_case")]
pub enum AutomationSelectionGranularity {
    #[default]
    ExactBase,
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
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum AutomationOptionScope {
    Workspace,
    Track { track_id: TrackId },
    Panel { panel_id: PanelId },
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
#[serde(tag = "selection", rename_all = "snake_case")]
pub enum AutomationSelection {
    Track {
        track_id: TrackId,
    },
    Panel {
        panel_id: PanelId,
    },
    Position {
        panel_id: PanelId,
        track_id: TrackId,
        position: u64,
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
#[serde(rename_all = "snake_case")]
pub enum AutomationSnapshotScope {
    #[default]
    VisibleWorkspace,
    ActivePanel,
    FullActivePanel,
    NativeWindow,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSnapshotFormat {
    #[default]
    Png,
    Svg,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationExportKind {
    Alignments,
    Pileup,
    SelectedEvidence,
    Modifications,
    Junctions,
    Variants,
    Quantitative,
    Regions,
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
                render_lanes: Vec::new(),
                read_panel_states: Vec::new(),
                selected_presentation_nodes: Vec::new(),
                presentation_selection_anchor: None,
            }],
            tracks: Vec::new(),
            read_pane_height: 360.0,
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
        assert!(!workspace_json.contains("source"));
        assert!(!workspace_json.contains("path"));
        assert!(!workspace_json.contains("sequence"));
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
}
